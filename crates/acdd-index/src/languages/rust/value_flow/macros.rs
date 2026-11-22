use super::types::parse_rust_type_text;
use crate::ast::text;
use crate::semantic::{SourcePosition, TypeExpr};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

#[derive(Clone, Default, Debug)]
pub(super) struct BlockMacroFacts {
    pub(super) defs: Vec<(usize, &'static str)>,
    pub(super) imports: HashSet<&'static str>,
}

#[derive(Clone, Default, Debug)]
pub(super) struct ModuleMacroFacts {
    pub(super) defs: Vec<(usize, &'static str)>,
    pub(super) imports: HashSet<&'static str>,
}

#[derive(Clone, Default, Debug)]
pub(super) struct FileMacroFacts {
    pub(super) root_macro_defs: Vec<(usize, &'static str)>,
    pub(super) root_macro_imports: HashSet<&'static str>,
    pub(super) modules: HashMap<usize, ModuleMacroFacts>,
    pub(super) blocks: HashMap<usize, BlockMacroFacts>,
    pub(super) thread_locals: Vec<(usize, String, SourcePosition, TypeExpr)>,
}

pub(super) fn get_file_root(mut node: Node<'_>) -> Node<'_> {
    while let Some(parent) = node.parent() {
        node = parent;
    }
    node
}

fn module_of<'a>(mut node: Node<'a>) -> Node<'a> {
    if node.kind() == "declaration_list"
        && node.parent().is_some_and(|p| p.kind() == "mod_item")
    {
        return node;
    }
    if node.kind() == "source_file" {
        return node;
    }
    while let Some(parent) = node.parent() {
        if parent.kind() == "declaration_list"
            && parent.parent().is_some_and(|gp| gp.kind() == "mod_item")
        {
            return parent;
        }
        node = parent;
    }
    node
}

fn parent_module<'a>(mod_node: Node<'a>, file_root: Node<'a>) -> Option<Node<'a>> {
    if mod_node == file_root {
        return None;
    }
    let mod_item = mod_node.parent()?;
    let enclosing = mod_item.parent()?;
    Some(module_of(enclosing))
}

fn find_child_module<'a>(body: Node<'a>, mod_name: &str, source: &str) -> Option<Node<'a>> {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "mod_item"
            && child
                .child_by_field_name("name")
                .is_some_and(|n| text(n, source).trim() == mod_name)
        {
            return child.child_by_field_name("body");
        }
    }
    None
}

fn resolve_module_path<'a, 's>(
    context: Node<'a>,
    path: &'s str,
    source: &str,
    file_root: Node<'a>,
) -> Option<(Node<'a>, &'s str)> {
    let enc_mod = module_of(context);
    let (mut current, clean_path) = if let Some(stripped) = path.strip_prefix("crate::") {
        (file_root, stripped)
    } else if let Some(stripped) = path.strip_prefix("self::") {
        (enc_mod, stripped)
    } else if path.starts_with("super::") {
        let mut curr_mod = enc_mod;
        let mut remaining = path;
        while let Some(stripped) = remaining.strip_prefix("super::") {
            curr_mod = parent_module(curr_mod, file_root)?;
            remaining = stripped;
        }
        (curr_mod, remaining)
    } else {
        let first_seg = path.split("::").next().unwrap_or(path);
        if find_child_module(enc_mod, first_seg, source).is_some() || !path.contains("::") {
            (enc_mod, path)
        } else {
            (file_root, path)
        }
    };

    let parts: Vec<&str> = clean_path.split("::").collect();
    let target_name = *parts.last()?;
    for &mod_name in &parts[..parts.len() - 1] {
        current = find_child_module(current, mod_name, source)?;
    }
    Some((current, target_name))
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum MacroResolution {
    KnownMacro,
    KnownNonMacro,
    UnknownOrExhausted,
}

fn item_matches_non_macro(item: Node<'_>, target_name: &str, source: &str) -> bool {
    match item.kind() {
        "function_item"
        | "function_signature_item"
        | "struct_item"
        | "enum_item"
        | "type_item"
        | "const_item"
        | "static_item"
        | "trait_item"
        | "union_item"
        | "mod_item" => item
            .child_by_field_name("name")
            .is_some_and(|n| text(n, source).trim() == target_name),
        "foreign_mod_item" => foreign_mod_contains_name(item, target_name, source),
        "extern_crate_declaration" => item
            .child_by_field_name("alias")
            .or_else(|| item.child_by_field_name("name"))
            .is_some_and(|n| text(n, source).trim() == target_name),
        _ => false,
    }
}

fn foreign_mod_contains_name(item: Node<'_>, target_name: &str, source: &str) -> bool {
    let mut f_cursor = item.walk();
    for f_child in item.named_children(&mut f_cursor) {
        if f_child.kind() == "declaration_list" {
            let mut d_cursor = f_child.walk();
            let hit = f_child.named_children(&mut d_cursor).any(|d_child| {
                d_child
                    .child_by_field_name("name")
                    .is_some_and(|n| text(n, source).trim() == target_name)
            });
            if hit {
                return true;
            }
        } else if f_child
            .child_by_field_name("name")
            .is_some_and(|n| text(n, source).trim() == target_name)
        {
            return true;
        }
    }
    false
}

fn check_reexported_macro(
    body: Node<'_>,
    item: Node<'_>,
    target_name: &str,
    source: &str,
    file_root: Node<'_>,
    visited: &mut HashSet<(usize, String)>,
    depth: usize,
) -> Option<MacroResolution> {
    let arg = item.child_by_field_name("argument")?;
    let mut mod_imports = Vec::new();
    super::super::collect_use(arg, source, "", &mut mod_imports);
    let mut saw_non_macro = false;
    let mut saw_exhausted = false;
    for (re_path, re_alias) in mod_imports {
        if re_alias != target_name {
            continue;
        }
        let clean = re_path
            .strip_prefix("crate::")
            .or_else(|| re_path.strip_prefix("self::"))
            .unwrap_or(&re_path);
        if clean.starts_with("std::") || clean.starts_with("core::") || clean.starts_with("alloc::")
        {
            saw_non_macro = true;
            continue;
        }
        let Some((target_body, actual_target)) =
            resolve_module_path(body, &re_path, source, file_root)
        else {
            saw_exhausted = true;
            continue;
        };
        match module_has_macro(
            target_body,
            actual_target,
            source,
            file_root,
            visited,
            depth + 1,
        ) {
            MacroResolution::KnownMacro => return Some(MacroResolution::KnownMacro),
            MacroResolution::KnownNonMacro => saw_non_macro = true,
            MacroResolution::UnknownOrExhausted => saw_exhausted = true,
        }
    }
    if saw_exhausted {
        Some(MacroResolution::UnknownOrExhausted)
    } else if saw_non_macro {
        Some(MacroResolution::KnownNonMacro)
    } else {
        None
    }
}

fn module_has_macro(
    body: Node<'_>,
    target_name: &str,
    source: &str,
    file_root: Node<'_>,
    visited: &mut HashSet<(usize, String)>,
    depth: usize,
) -> MacroResolution {
    if depth > 8 || !visited.insert((body.id(), target_name.to_string())) {
        return MacroResolution::UnknownOrExhausted;
    }
    let mut saw_exhausted = false;
    let mut saw_non_macro = false;
    let mut cursor = body.walk();
    for item in body.named_children(&mut cursor) {
        if item.kind() == "macro_definition" {
            if item
                .child_by_field_name("name")
                .is_some_and(|n| text(n, source).trim() == target_name)
            {
                return MacroResolution::KnownMacro;
            }
        } else if item_matches_non_macro(item, target_name, source) {
            saw_non_macro = true;
        } else if item.kind() == "use_declaration" {
            match check_reexported_macro(body, item, target_name, source, file_root, visited, depth)
            {
                Some(MacroResolution::KnownMacro) => return MacroResolution::KnownMacro,
                Some(MacroResolution::KnownNonMacro) => saw_non_macro = true,
                Some(MacroResolution::UnknownOrExhausted) => saw_exhausted = true,
                None => {}
            }
        }
    }
    if saw_non_macro && !saw_exhausted {
        MacroResolution::KnownNonMacro
    } else {
        MacroResolution::UnknownOrExhausted
    }
}

fn is_imported_target_macro(
    context: Node<'_>,
    path: &str,
    source: &str,
    file_root: Node<'_>,
) -> bool {
    let clean = path
        .strip_prefix("crate::")
        .or_else(|| path.strip_prefix("self::"))
        .unwrap_or(path);
    if clean.starts_with("std::") || clean.starts_with("core::") || clean.starts_with("alloc::") {
        return false;
    }
    if let Some((target_body, target_name)) = resolve_module_path(context, path, source, file_root)
    {
        let mut visited = HashSet::new();
        let res = module_has_macro(target_body, target_name, source, file_root, &mut visited, 0);
        return matches!(
            res,
            MacroResolution::KnownMacro | MacroResolution::UnknownOrExhausted
        );
    }
    true
}

fn collect_scope_macro_facts<'tree>(
    scope: Node<'tree>,
    file_root: Node<'tree>,
    source: &str,
    defs: &mut Vec<(usize, &'static str)>,
    imports: &mut HashSet<&'static str>,
    thread_locals: &mut Vec<(usize, String, SourcePosition, TypeExpr)>,
    stack: &mut Vec<Node<'tree>>,
) {
    let mut cursor = scope.walk();
    for child in scope.named_children(&mut cursor) {
        match child.kind() {
            "macro_invocation" => {
                collect_thread_local_from_macro(child, scope.id(), source, thread_locals);
                stack.push(child);
            }
            "macro_definition" => {
                if let Some(m_name) = child.child_by_field_name("name") {
                    match text(m_name, source).trim() {
                        "vec" => defs.push((child.end_byte(), "vec")),
                        "format" => defs.push((child.end_byte(), "format")),
                        _ => {}
                    }
                }
            }
            "use_declaration" => {
                collect_use_macro_shadows(child, scope, file_root, source, imports);
            }
            _ => stack.push(child),
        }
    }
}

fn collect_thread_local_from_macro(
    child: Node<'_>,
    scope_id: usize,
    source: &str,
    thread_locals: &mut Vec<(usize, String, SourcePosition, TypeExpr)>,
) {
    let Some(m_name) = child.child_by_field_name("macro") else {
        return;
    };
    if !matches!(text(m_name, source).trim(), "thread_local" | "std::thread_local") {
        return;
    }
    let Some(tree) = child
        .named_children(&mut child.walk())
        .find(|c| c.kind() == "token_tree")
    else {
        return;
    };
    for (name, pos, ty_expr) in parse_thread_local_statics(tree, source) {
        thread_locals.push((scope_id, name, pos, ty_expr));
    }
}

fn collect_use_macro_shadows(
    child: Node<'_>,
    scope: Node<'_>,
    file_root: Node<'_>,
    source: &str,
    imports: &mut HashSet<&'static str>,
) {
    let Some(argument) = child.child_by_field_name("argument") else {
        return;
    };
    let mut use_imports = Vec::new();
    super::super::collect_use(argument, source, "", &mut use_imports);
    for (path, alias) in use_imports {
        if (alias == "vec" || alias == "format")
            && !path.starts_with("std::")
            && !path.starts_with("core::")
            && !path.starts_with("alloc::")
            && is_imported_target_macro(scope, &path, source, file_root)
        {
            imports.insert(if alias == "vec" { "vec" } else { "format" });
        }
    }
}

fn build_file_macro_facts(root: Node<'_>, source: &str) -> FileMacroFacts {
    let file_root = get_file_root(root);
    let mut facts = FileMacroFacts::default();
    let mut stack = vec![file_root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node == file_root {
            collect_scope_macro_facts(
                node,
                file_root,
                source,
                &mut facts.root_macro_defs,
                &mut facts.root_macro_imports,
                &mut facts.thread_locals,
                &mut stack,
            );
        } else if kind == "declaration_list" {
            let mut mod_facts = ModuleMacroFacts::default();
            collect_scope_macro_facts(
                node,
                file_root,
                source,
                &mut mod_facts.defs,
                &mut mod_facts.imports,
                &mut facts.thread_locals,
                &mut stack,
            );
            facts.modules.insert(node.id(), mod_facts);
        } else if kind == "block" {
            let mut block_facts = BlockMacroFacts::default();
            collect_scope_macro_facts(
                node,
                file_root,
                source,
                &mut block_facts.defs,
                &mut block_facts.imports,
                &mut facts.thread_locals,
                &mut stack,
            );
            facts.blocks.insert(node.id(), block_facts);
        } else {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
    }
    facts
}

std::thread_local! {
    static CACHED_FILE_FACTS: std::cell::RefCell<Option<(usize, std::sync::Arc<FileMacroFacts>)>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn clear_file_macro_facts() {
    CACHED_FILE_FACTS.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

pub(super) fn file_macro_facts(root: Node<'_>, source: &str) -> std::sync::Arc<FileMacroFacts> {
    let file_root = get_file_root(root);
    let root_id = file_root.id();
    CACHED_FILE_FACTS.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if let Some((cached_id, ref facts)) = *borrow {
            if cached_id == root_id {
                return facts.clone();
            }
        }
        let facts = std::sync::Arc::new(build_file_macro_facts(file_root, source));
        *borrow = Some((root_id, facts.clone()));
        facts
    })
}

fn extend_scope_shadows(
    shadows: &mut HashSet<&'static str>,
    imports: &HashSet<&'static str>,
    defs: &[(usize, &'static str)],
    node_start_byte: usize,
) {
    shadows.extend(imports);
    for (end_byte, name) in defs {
        if *end_byte <= node_start_byte {
            shadows.insert(name);
        }
    }
}

pub(super) fn collect_ancestor_shadows(
    node: Node<'_>,
    _source: &str,
    file_facts: &FileMacroFacts,
) -> HashSet<&'static str> {
    let start_byte = node.start_byte();
    let mut shadows = HashSet::new();
    extend_scope_shadows(
        &mut shadows,
        &file_facts.root_macro_imports,
        &file_facts.root_macro_defs,
        start_byte,
    );
    let file_root = get_file_root(node);
    let mut curr = node;
    while let Some(parent) = curr.parent() {
        if parent == file_root {
            break;
        }
        if parent.kind() == "declaration_list"
            && parent.parent().is_some_and(|gp| gp.kind() == "mod_item")
        {
            if let Some(mod_facts) = file_facts.modules.get(&parent.id()) {
                extend_scope_shadows(&mut shadows, &mod_facts.imports, &mod_facts.defs, start_byte);
            }
        } else if parent.kind() == "block" {
            if let Some(block_facts) = file_facts.blocks.get(&parent.id()) {
                extend_scope_shadows(
                    &mut shadows,
                    &block_facts.imports,
                    &block_facts.defs,
                    start_byte,
                );
            }
        }
        curr = parent;
    }
    shadows
}

pub(super) fn parse_thread_local_statics(
    tree: Node<'_>,
    source: &str,
) -> Vec<(String, SourcePosition, TypeExpr)> {
    let raw = text(tree, source);
    let base_pos = tree.start_position();
    let mut out = Vec::new();
    for stmt in raw.split(';') {
        let Some(static_pos) = stmt.find("static ") else {
            continue;
        };
        let rest = stmt[static_pos + "static ".len()..].trim_start();
        let Some((name_part, rhs)) = rest.split_once(':') else {
            continue;
        };
        let name = name_part.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Some((ty_part, _)) = rhs.split_once('=') else {
            continue;
        };
        let ty = parse_rust_type_text(ty_part.trim(), 0);
        if !matches!(ty, TypeExpr::Unknown) {
            out.push((
                name.to_owned(),
                SourcePosition {
                    line: base_pos.row + 1,
                    column: base_pos.column,
                },
                ty,
            ));
        }
    }
    out
}
