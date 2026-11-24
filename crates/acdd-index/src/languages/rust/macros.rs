use crate::ast::syntax::text;
use crate::languages::types::SyntaxContext;
use crate::models::{Edge, Facts, Node};
use serde_json::json;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node as TsNode;

const MAX_MACRO_ITEMS: usize = 128;
const MAX_TREE_DEPTH: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MacroItemKind {
    Struct,
    Enum,
    Function,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MacroVisibility {
    Public,
    Private,
    VisParam,
}

impl MacroVisibility {
    fn resolve(self, invocation_public: bool) -> bool {
        match self {
            Self::Public => true,
            Self::Private => false,
            Self::VisParam => invocation_public,
        }
    }
}

#[derive(Clone)]
struct MacroMethodSpec {
    name: String,
    is_static: bool,
    visibility: MacroVisibility,
    synth_return: Option<&'static str>,
}

#[derive(Clone)]
struct MacroConstSpec {
    name: String,
    visibility: MacroVisibility,
}

#[derive(Clone)]
struct MacroTemplate {
    name: String,
    kind: MacroItemKind,
    visibility: MacroVisibility,
    has_variant_repetition: bool,
    derives: Vec<&'static str>,
    methods: Vec<MacroMethodSpec>,
    consts: Vec<MacroConstSpec>,
    has_from_str: bool,
}

thread_local! {
    static FILE_MACRO_TEMPLATES: RefCell<Option<(usize, HashMap<String, MacroTemplate>)>> =
        const { RefCell::new(None) };
}

pub(super) fn prepare(root: TsNode<'_>, source: &str) {
    let templates = collect_templates(root, source);
    let root_id = root.id();
    FILE_MACRO_TEMPLATES.with(|cell| {
        *cell.borrow_mut() = Some((root_id, templates));
    });
}

pub(super) fn clear() {
    FILE_MACRO_TEMPLATES.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

fn canonical_derive_trait(raw: &str) -> Option<&'static str> {
    let short = raw.trim().rsplit("::").next()?.trim();
    match short {
        "Default" => Some("Default"),
        "Clone" => Some("Clone"),
        "Copy" => Some("Copy"),
        "Debug" => Some("Debug"),
        "PartialEq" => Some("PartialEq"),
        "Eq" => Some("Eq"),
        "PartialOrd" => Some("PartialOrd"),
        "Ord" => Some("Ord"),
        "Hash" => Some("Hash"),
        "Serialize" => Some("Serialize"),
        "Deserialize" => Some("Deserialize"),
        "Parser" => Some("Parser"),
        _ => None,
    }
}

fn has_clap_parser_signal(node: TsNode<'_>, source: &str) -> bool {
    let mut has_explicit_clap = false;
    let mut has_clap_attr = false;
    for_each_preceding_attribute(node, |sibling| {
        let raw = text(sibling, source);
        let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("clap::Parser")
            || compact.starts_with("#[command(")
            || compact.starts_with("#[clap(")
        {
            has_clap_attr = true;
            if compact.contains("clap::Parser") {
                has_explicit_clap = true;
            }
        }
    });
    if has_explicit_clap || has_clap_attr {
        return true;
    }
    let body_text = text(node, source);
    if body_text.contains("#[command(") || body_text.contains("#[arg(") || body_text.contains("#[clap(") {
        return true;
    }
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut cursor = root.walk();
    let found = root.named_children(&mut cursor).any(|child| {
        child.kind() == "use_declaration" && {
            let txt = text(child, source);
            txt.contains("clap::") && txt.contains("Parser")
        }
    });
    found
}

fn parse_derive_inner(inner: &str, out: &mut Vec<&'static str>) {
    for part in inner.split(',') {
        if let Some(canonical) = canonical_derive_trait(part) {
            if !out.contains(&canonical) {
                out.push(canonical);
            }
        }
    }
}

fn parse_derive_attribute_text(attr: &str, out: &mut Vec<&'static str>) {
    let trimmed = attr.trim();
    let Some(body) = trimmed.strip_prefix("#[").and_then(|s| s.strip_suffix(']')) else {
        return;
    };
    let body = body.trim();
    if let Some(rest) = body
        .strip_prefix("derive")
        .map(str::trim_start)
        .and_then(|s| s.strip_prefix('('))
        .and_then(|s| s.strip_suffix(')'))
    {
        parse_derive_inner(rest, out);
    }
}

pub(super) struct SyntheticOwner<'a> {
    pub id: &'a str,
    pub qualname: &'a str,
    pub name: &'a str,
    pub path: &'a str,
    pub line: usize,
    pub end_line: usize,
    pub is_public: bool,
    pub is_test: bool,
}

pub(super) fn for_each_preceding_attribute(node: TsNode<'_>, mut f: impl FnMut(TsNode<'_>)) {
    let mut previous = node.prev_named_sibling();
    while let Some(sibling) = previous {
        if sibling.kind() == "attribute_item" {
            f(sibling);
        } else if !sibling.kind().contains("comment") {
            break;
        }
        previous = sibling.prev_named_sibling();
    }
}

pub(super) fn extract_derives(node: TsNode<'_>, source: &str) -> Vec<&'static str> {
    let mut derives = Vec::new();
    for_each_preceding_attribute(node, |sibling| {
        parse_derive_attribute_text(text(sibling, source), &mut derives);
    });
    if derives.contains(&"Parser") && !has_clap_parser_signal(node, source) {
        derives.retain(|trait_name| *trait_name != "Parser");
    }
    derives
}

fn derive_methods(trait_name: &str) -> &'static [(&'static str, bool, Option<&'static str>)] {
    match trait_name {
        "Default" => &[("default", true, Some("Self"))],
        "Clone" => &[("clone", false, Some("Self")), ("clone_from", false, None)],
        "Debug" => &[("fmt", false, None)],
        "PartialEq" => &[("eq", false, None), ("ne", false, None)],
        "PartialOrd" => &[
            ("partial_cmp", false, None),
            ("lt", false, None),
            ("le", false, None),
            ("gt", false, None),
            ("ge", false, None),
        ],
        "Ord" => &[
            ("cmp", false, None),
            ("max", false, Some("Self")),
            ("min", false, Some("Self")),
            ("clamp", false, Some("Self")),
        ],
        "Hash" => &[("hash", false, None)],
        "Serialize" => &[("serialize", false, None)],
        "Deserialize" => &[("deserialize", true, Some("Result<Self>"))],
        "Parser" => &[
            ("parse", true, Some("Self")),
            ("try_parse", true, Some("Result<Self>")),
            ("parse_from", true, Some("Self")),
            ("try_parse_from", true, Some("Result<Self>")),
            ("command", true, None),
            ("update_from_arg_matches", false, Some("Result<Self>")),
        ],
        _ => &[],
    }
}

pub(super) fn synthesize_derive_methods(
    owner: &SyntheticOwner<'_>,
    derives: &[&str],
    facts: &mut Facts,
) {
    if derives.is_empty() {
        return;
    }
    let mut existing: HashSet<String> = facts
        .nodes
        .iter()
        .filter(|node| {
            node.kind == crate::models::tags::NodeKind::Method
                && node
                    .qualname
                    .rsplit_once('.')
                    .is_some_and(|(scope, _)| scope == owner.qualname)
        })
        .map(|node| node.name.clone())
        .collect();
    for trait_name in derives {
        for &(method_name, is_static, synth_return) in derive_methods(trait_name) {
            if !existing.insert(method_name.to_owned()) {
                continue;
            }
            push_synthetic_method(
                facts,
                owner,
                method_name,
                is_static,
                owner.is_public,
                synth_return,
                Some(trait_name),
            );
        }
    }
}

fn push_synthetic_method(
    facts: &mut Facts,
    owner: &SyntheticOwner<'_>,
    method_name: &str,
    is_static: bool,
    is_public: bool,
    synth_return: Option<&str>,
    derive_trait: Option<&str>,
) {
    let id = format!(
        "rs:{}:{}:{}::{method_name}",
        owner.path, owner.line, owner.name
    );
    if facts.nodes.iter().any(|node| node.id == id) {
        return;
    }
    let mut details = serde_json::Map::new();
    details.insert("is_public".into(), json!(is_public));
    details.insert("is_method".into(), json!(true));
    details.insert("is_static".into(), json!(is_static));
    details.insert("receiver_type".into(), json!(owner.name));
    details.insert("synthesized".into(), json!(true));
    if let Some(ret) = synth_return {
        details.insert("rust_synth_return".into(), json!(ret));
    }
    if let Some(trait_name) = derive_trait {
        details.insert("rust_derive_trait".into(), json!(trait_name));
    }
    facts.nodes.push(Node {
        native_semantic: None,
        id: id.clone(),
        kind: crate::models::tags::NodeKind::Method,
        name: method_name.into(),
        qualname: format!("{}.{method_name}", owner.qualname),
        path: owner.path.into(),
        line: owner.line,
        end_line: owner.line,
        is_test: owner.is_test,
        language: crate::models::tags::LanguageId::Rust,
        generated: true,
        details: serde_json::Value::Object(details),
    });
    facts.edges.push(Edge {
        src: owner.id.into(),
        dst: id,
        kind: "contains".into(),
        path: owner.path.into(),
        line: owner.line,
        evidence: derive_trait
            .map_or_else(|| "macro_rules".to_owned(), |_| "derive".to_owned()),
        confidence: "exact".into(),
    });
}

fn push_synthetic_constant(
    facts: &mut Facts,
    owner: &SyntheticOwner<'_>,
    name: impl AsRef<str>,
    is_public: bool,
    is_enum_variant: bool,
) {
    let name = name.as_ref();
    let const_id = format!("rs:{}:{}:{}::{name}", owner.path, owner.line, owner.name);
    let mut details = json!({
        "is_public": is_public,
        "is_static": true,
    });
    if is_enum_variant {
        details["rust_enum_variant"] = json!(true);
    }
    facts.nodes.push(Node {
        native_semantic: None,
        id: const_id.clone(),
        kind: crate::models::tags::NodeKind::Constant,
        name: name.to_owned(),
        qualname: format!("{}.{name}", owner.qualname),
        path: owner.path.to_owned(),
        line: owner.line,
        end_line: owner.end_line,
        is_test: owner.is_test,
        language: crate::models::tags::LanguageId::Rust,
        generated: true,
        details,
    });
    facts.edges.push(Edge {
        src: owner.id.to_owned(),
        dst: const_id,
        kind: "contains".into(),
        path: owner.path.to_owned(),
        line: owner.line,
        evidence: "macro_rules".into(),
        confidence: "exact".into(),
    });
}

fn collect_templates(root: TsNode<'_>, source: &str) -> HashMap<String, MacroTemplate> {
    let mut map = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "macro_definition" {
            if let Some(template) = parse_macro_definition(node, source) {
                map.entry(template.name.clone()).or_insert(template);
            }
            continue;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(
                child.kind(),
                "declaration_list" | "mod_item" | "macro_definition"
            ) {
                stack.push(child);
            }
        }
    }
    map
}

fn contains_macro_self_call(tree: TsNode<'_>, macro_name: &str, source: &str, depth: usize) -> bool {
    if depth > MAX_TREE_DEPTH {
        return false;
    }
    let mut cursor = tree.walk();
    let children: Vec<_> = tree.children(&mut cursor).collect();
    for (idx, child) in children.iter().enumerate() {
        if child.kind() == "token_tree" {
            if contains_macro_self_call(*child, macro_name, source, depth + 1) {
                return true;
            }
            continue;
        }
        if text(*child, source).trim() == macro_name
            && children
                .get(idx + 1)
                .is_some_and(|next| text(*next, source).trim().starts_with('!'))
        {
            return true;
        }
    }
    false
}

fn left_starts_with_at_tag(left: TsNode<'_>, source: &str) -> bool {
    let mut cursor = left.walk();
    for child in left.children(&mut cursor) {
        let tok = text(child, source).trim();
        if tok.is_empty() || matches!(tok, "(" | "[" | "{") || child.kind().contains("comment") {
            continue;
        }
        return tok == "@" || tok.starts_with('@');
    }
    false
}

fn parse_macro_definition(node: TsNode<'_>, source: &str) -> Option<MacroTemplate> {
    let name = text(node.child_by_field_name("name")?, source).trim();
    if name.is_empty() {
        return None;
    }
    let mut cursor = node.walk();
    let rules: Vec<_> = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "macro_rule")
        .collect();
    if rules.iter().any(|rule| {
        rule.child_by_field_name("left")
            .is_some_and(|left| left_starts_with_at_tag(left, source))
            || rule
                .child_by_field_name("right")
                .is_some_and(|right| contains_macro_self_call(right, name, source, 0))
    }) {
        return None;
    }
    for rule in rules {
        let Some(left) = rule.child_by_field_name("left") else {
            continue;
        };
        let Some(right) = rule.child_by_field_name("right") else {
            continue;
        };
        if let Some(template) = parse_macro_rule(name, left, right, source) {
            return Some(template);
        }
    }
    None
}

fn collect_rule_metavars(
    node: TsNode<'_>,
    source: &str,
    depth: usize,
    in_rep: bool,
    top_idents: &mut Vec<String>,
    rep_idents: &mut Vec<String>,
    vis_params: &mut Vec<String>,
) {
    if depth > MAX_TREE_DEPTH {
        return;
    }
    if node.kind() == "token_binding_pattern" {
        if let (Some(name_n), Some(type_n)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("type"),
        ) {
            let var = text(name_n, source)
                .trim()
                .trim_start_matches('$')
                .to_owned();
            match text(type_n, source).trim() {
                "ident" => {
                    if in_rep {
                        rep_idents.push(var);
                    } else {
                        top_idents.push(var);
                    }
                }
                "vis" if !in_rep => vis_params.push(var),
                _ => {}
            }
        }
        return;
    }
    let next_rep = in_rep || node.kind() == "token_repetition_pattern";
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rule_metavars(
            child, source, depth + 1, next_rep, top_idents, rep_idents, vis_params,
        );
    }
}

fn token_at_child_is<'a>(children: &[TsNode<'a>], idx: usize, source: &'a str) -> &'a str {
    children
        .get(idx)
        .map_or("", |node| text(*node, source).trim())
}

fn visibility_before(
    children: &[TsNode<'_>],
    kw_idx: usize,
    source: &str,
    vis_params: &[String],
) -> MacroVisibility {
    if kw_idx > 0 {
        let prev = token_at_child_is(children, kw_idx - 1, source);
        if prev == "pub" {
            return MacroVisibility::Public;
        }
        if let Some(var) = prev.strip_prefix('$') {
            if vis_params.iter().any(|p| p == var) {
                return MacroVisibility::VisParam;
            }
        } else if kw_idx > 1
            && token_at_child_is(children, kw_idx - 2, source) == "$"
            && vis_params.iter().any(|p| p == prev)
        {
            return MacroVisibility::VisParam;
        }
    }
    MacroVisibility::Private
}

fn scan_macro_item_header(
    children: &[TsNode<'_>],
    source: &str,
    top_idents: &[String],
    rep_idents: &[String],
    vis_params: &[String],
    derives: &mut Vec<&'static str>,
) -> Option<(MacroItemKind, String, MacroVisibility, bool)> {
    let mut item_kind = None;
    let mut type_param = None;
    let mut visibility = MacroVisibility::Private;
    let mut has_variant_repetition = false;

    for (idx, child) in children.iter().enumerate() {
        let tok = text(*child, source).trim();
        if tok == "#" {
            if let Some(attr_tree) = children.get(idx + 1).filter(|n| n.kind() == "token_tree") {
                let raw_attr = format!("#{}", text(*attr_tree, source));
                parse_derive_attribute_text(&raw_attr, derives);
            }
        }
        if item_kind.is_some() || !matches!(tok, "struct" | "enum" | "fn") {
            continue;
        }
        let raw_next = token_at_child_is(children, idx + 1, source);
        let next_tok = raw_next.trim_start_matches('$');
        if !top_idents.iter().any(|var| var == next_tok) {
            continue;
        }
        item_kind = Some(match tok {
            "struct" => MacroItemKind::Struct,
            "enum" => MacroItemKind::Enum,
            _ => MacroItemKind::Function,
        });
        type_param = Some(raw_next.to_owned());
        visibility = visibility_before(children, idx, source, vis_params);
        if tok == "enum" {
            has_variant_repetition = children
                .get(idx + 2)
                .filter(|n| n.kind() == "token_tree")
                .is_some_and(|body| contains_rep_ident(*body, rep_idents, source, 0));
        }
    }
    Some((item_kind?, type_param?, visibility, has_variant_repetition))
}

fn collect_macro_impl_members(
    children: &[TsNode<'_>],
    source: &str,
    type_param: &str,
    visibility: MacroVisibility,
    vis_params: &[String],
) -> (Vec<MacroMethodSpec>, Vec<MacroConstSpec>, bool) {
    let mut methods = Vec::new();
    let mut consts = Vec::new();
    let mut has_from_str = false;

    for (idx, child) in children.iter().enumerate() {
        if text(*child, source).trim() != "impl" {
            continue;
        }
        let mut cursor_idx = idx + 1;
        while cursor_idx < children.len() {
            let tok = token_at_child_is(children, cursor_idx, source);
            if tok == type_param {
                let is_display = children[(idx + 1)..cursor_idx].iter().any(|scan| {
                    let st = text(*scan, source).trim();
                    st == "Display" || st.ends_with("::Display") || st == "ToString"
                });
                let has_for = children[(idx + 1)..cursor_idx]
                    .iter()
                    .any(|scan| text(*scan, source).trim() == "for");
                if has_for
                    && is_display
                    && !methods
                        .iter()
                        .any(|existing: &MacroMethodSpec| existing.name == "to_string")
                {
                    methods.push(MacroMethodSpec {
                        name: "to_string".to_owned(),
                        is_static: false,
                        visibility,
                        synth_return: Some("String"),
                    });
                }
                if let Some(body) = children
                    .get(cursor_idx + 1)
                    .filter(|n| n.kind() == "token_tree" && text(**n, source).trim().starts_with('{'))
                {
                    extract_impl_members(*body, source, vis_params, &mut methods, &mut consts);
                }
                break;
            }
            if tok == "FromStr" {
                has_from_str |= ((cursor_idx + 1)..children.len().min(cursor_idx + 6))
                    .map(|scan| token_at_child_is(children, scan, source))
                    .take_while(|st| !st.starts_with('{'))
                    .any(|st| st == type_param);
            }
            if children[cursor_idx].kind() == "token_tree"
                && text(children[cursor_idx], source).trim().starts_with('{')
            {
                break;
            }
            cursor_idx += 1;
        }
    }
    (methods, consts, has_from_str)
}

fn parse_macro_rule(
    macro_name: &str,
    left: TsNode<'_>,
    right: TsNode<'_>,
    source: &str,
) -> Option<MacroTemplate> {
    let mut top_idents = Vec::new();
    let mut rep_idents = Vec::new();
    let mut vis_params = Vec::new();
    collect_rule_metavars(
        left,
        source,
        0,
        false,
        &mut top_idents,
        &mut rep_idents,
        &mut vis_params,
    );
    if top_idents.is_empty() {
        return None;
    }

    let mut cursor = right.walk();
    let children: Vec<_> = right.children(&mut cursor).collect();
    let mut derives = Vec::new();
    let (item_kind, type_param, visibility, has_variant_repetition) = scan_macro_item_header(
        &children,
        source,
        &top_idents,
        &rep_idents,
        &vis_params,
        &mut derives,
    )?;
    let (methods, consts, has_from_str) = if item_kind == MacroItemKind::Function {
        (Vec::new(), Vec::new(), false)
    } else {
        collect_macro_impl_members(&children, source, &type_param, visibility, &vis_params)
    };

    Some(MacroTemplate {
        name: macro_name.to_owned(),
        kind: item_kind,
        visibility,
        has_variant_repetition,
        derives,
        methods,
        consts,
        has_from_str,
    })
}

fn contains_rep_ident(
    node: TsNode<'_>,
    rep_idents: &[String],
    source: &str,
    depth: usize,
) -> bool {
    if depth > MAX_TREE_DEPTH || rep_idents.is_empty() {
        return false;
    }
    if node.kind() == "token_repetition" {
        let rep_text = text(node, source);
        return rep_idents
            .iter()
            .any(|var| rep_text.contains(&format!("${var}")) || rep_text.contains(var));
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|child| contains_rep_ident(child, rep_idents, source, depth + 1));
    found
}

fn extract_impl_members(
    body: TsNode<'_>,
    source: &str,
    vis_params: &[String],
    methods: &mut Vec<MacroMethodSpec>,
    consts: &mut Vec<MacroConstSpec>,
) {
    let mut cursor = body.walk();
    let children: Vec<_> = body.children(&mut cursor).collect();
    let mut idx = 0;
    while idx < children.len() {
        let tok = token_at_child_is(&children, idx, source);
        if tok == "const" {
            let next_tok = token_at_child_is(&children, idx + 1, source);
            if next_tok != "fn" && is_plain_ident(next_tok) {
                consts.push(MacroConstSpec {
                    name: next_tok.to_owned(),
                    visibility: visibility_before(&children, idx, source, vis_params),
                });
            }
        } else if tok == "fn" {
            let fn_name = token_at_child_is(&children, idx + 1, source);
            if is_plain_ident(fn_name) {
                let qualifier_idx = if idx > 0
                    && matches!(
                        token_at_child_is(&children, idx - 1, source),
                        "const" | "async"
                    ) {
                    idx - 1
                } else {
                    idx
                };
                let visibility = visibility_before(&children, qualifier_idx, source, vis_params);
                let mut scan = idx + 2;
                let mut params_node = None;
                while scan < children.len() && scan < idx + 6 {
                    if children[scan].kind() == "token_tree"
                        && text(children[scan], source).trim().starts_with('(')
                    {
                        params_node = Some(children[scan]);
                        break;
                    }
                    if text(children[scan], source).trim().starts_with('{') {
                        break;
                    }
                    scan += 1;
                }
                let is_static = params_node.is_none_or(|params| {
                    let p = text(params, source);
                    let inner = p
                        .trim()
                        .strip_prefix('(')
                        .and_then(|s| s.strip_suffix(')'))
                        .unwrap_or("")
                        .trim_start();
                    !(inner.starts_with("&self")
                        || inner.starts_with("&mut self")
                        || inner.starts_with("self")
                        || inner.starts_with("mut self"))
                });
                let synth_return = if let Some(params_pos) = params_node.map(|_| scan) {
                    parse_synth_return(&children[params_pos + 1..], source)
                } else {
                    None
                };
                if !methods.iter().any(|existing| existing.name == fn_name) {
                    methods.push(MacroMethodSpec {
                        name: fn_name.to_owned(),
                        is_static,
                        visibility,
                        synth_return,
                    });
                }
            }
        }
        idx += 1;
    }
}

fn parse_synth_return(rest: &[TsNode<'_>], source: &str) -> Option<&'static str> {
    let mut arrow_pos = None;
    for (idx, node) in rest.iter().take(6).enumerate() {
        let tok = text(*node, source).trim();
        if tok == "->" {
            arrow_pos = Some(idx);
            break;
        }
        if tok.starts_with('{') || tok == ";" {
            return None;
        }
    }
    let arrow = arrow_pos?;
    let mut ret_tokens = String::new();
    for node in rest.iter().skip(arrow + 1).take(12) {
        let tok = text(*node, source).trim();
        if tok.starts_with('{') || tok == "where" || tok == ";" {
            break;
        }
        if !ret_tokens.is_empty() {
            ret_tokens.push(' ');
        }
        ret_tokens.push_str(tok);
    }
    let compact: String = ret_tokens.chars().filter(|c| !c.is_whitespace()).collect();
    if compact == "Self" {
        Some("Self")
    } else if compact.starts_with("Result<Self") {
        Some("Result<Self>")
    } else if compact.starts_with("Option<Self") {
        Some("Option<Self>")
    } else if compact == "&str" || compact == "&'staticstr" {
        Some("str")
    } else if compact == "String" {
        Some("String")
    } else if compact == "bool" {
        Some("bool")
    } else if compact == "usize" {
        Some("usize")
    } else {
        None
    }
}

fn is_plain_ident(tok: &str) -> bool {
    !tok.is_empty()
        && !matches!(
            tok,
            "pub" | "crate" | "super" | "self" | "Self" | "struct" | "enum" | "impl" | "fn" | "const"
        )
        && tok.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
        && tok.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn next_macro_token<'a>(
    children: &[TsNode<'a>],
    idx: &mut usize,
    source: &'a str,
) -> Option<&'a str> {
    while *idx < children.len() {
        let child = children[*idx];
        let kind = child.kind();
        if kind.contains("comment") || matches!(kind, "(" | ")" | "{" | "}" | "[" | "]") {
            *idx += 1;
            continue;
        }
        let tok = text(child, source).trim();
        if skip_attr_token(children, tok, idx) {
            continue;
        }
        return Some(tok);
    }
    None
}

fn extract_invocation_name_and_variants(
    args_tree: TsNode<'_>,
    source: &str,
    extract_variants: bool,
) -> Option<(String, bool, Vec<String>)> {
    let mut cursor = args_tree.walk();
    let children: Vec<_> = args_tree.children(&mut cursor).collect();
    let mut item_name = None;
    let mut invocation_public = false;
    let mut idx = 0;
    while let Some(tok) = next_macro_token(&children, &mut idx, source) {
        if tok == "pub" {
            let scoped = children
                .get(idx + 1)
                .is_some_and(|next| next.kind() == "token_tree" && text(*next, source).trim().starts_with('('));
            invocation_public = !scoped;
            idx += 1;
            continue;
        }
        if is_plain_ident(tok) {
            item_name = Some(tok.to_owned());
            idx += 1;
            break;
        }
        idx += 1;
    }
    let item_name = item_name?;
    let mut variants = Vec::new();
    if extract_variants {
        for child in &children[idx..] {
            if child.kind() == "token_tree" && text(*child, source).trim().starts_with('{') {
                collect_enum_variants(*child, source, &mut variants);
                break;
            }
        }
    }
    Some((item_name, invocation_public, variants))
}

fn skip_attr_token(children: &[TsNode<'_>], tok: &str, idx: &mut usize) -> bool {
    if tok != "#" {
        return false;
    }
    *idx += 1;
    if *idx < children.len() && children[*idx].kind() == "token_tree" {
        *idx += 1;
    }
    true
}

fn collect_enum_variants(body: TsNode<'_>, source: &str, out: &mut Vec<String>) {
    let mut cursor = body.walk();
    let children: Vec<_> = body.children(&mut cursor).collect();
    let mut expect_variant = true;
    let mut idx = 0;
    while idx < children.len() && out.len() < MAX_MACRO_ITEMS {
        let child = children[idx];
        let kind = child.kind();
        if kind.contains("comment") {
            idx += 1;
            continue;
        }
        let tok = text(child, source).trim();
        if matches!(tok, "{" | "}" | "") {
            idx += 1;
            continue;
        }
        if skip_attr_token(&children, tok, &mut idx) {
            continue;
        }
        if tok == "," {
            expect_variant = true;
            idx += 1;
            continue;
        }
        if expect_variant && is_plain_ident(tok) {
            if !out.iter().any(|existing| existing == tok) {
                out.push(tok.to_owned());
            }
            expect_variant = false;
        }
        idx += 1;
    }
}

fn push_transcription_impl_traits(tree: TsNode<'_>, source: &str, out: &mut Vec<String>) {
    let mut cursor = tree.walk();
    let children: Vec<_> = tree.children(&mut cursor).collect();
    let mut tokens = Vec::new();
    for child in &children {
        if child.kind() == "token_tree" {
            push_transcription_impl_traits(*child, source, out);
        } else if !child.kind().contains("comment")
            && !matches!(
                child.kind(),
                "string_literal" | "raw_string_literal" | "char_literal"
            )
        {
            let tok = text(*child, source).trim();
            if !tok.is_empty() {
                tokens.push(tok);
            }
        }
    }
    let mut idx = 0;
    while idx < tokens.len() {
        let prev_path_sep = idx > 0 && matches!(tokens[idx - 1], "." | "::");
        if tokens[idx] != "impl" || prev_path_sep {
            idx += 1;
            continue;
        }
        let mut cursor_idx = idx + 1;
        if tokens.get(cursor_idx).copied() == Some("<") {
            let mut angle = 1usize;
            cursor_idx += 1;
            while cursor_idx < tokens.len() && angle > 0 {
                match tokens[cursor_idx] {
                    "<" => angle += 1,
                    ">" => angle -= 1,
                    _ => {}
                }
                cursor_idx += 1;
            }
        }
        let mut trait_path = String::new();
        let mut generic_depth = 0usize;
        let mut aborted = false;
        while cursor_idx < tokens.len() {
            let tok = tokens[cursor_idx];
            if generic_depth == 0 && tok == "for" {
                break;
            }
            if generic_depth == 0 && matches!(tok, ";" | "{" | "=" | "fn" | "type" | "const") {
                aborted = true;
                break;
            }
            match tok {
                "<" => generic_depth += 1,
                ">" => generic_depth = generic_depth.saturating_sub(1),
                _ if generic_depth == 0 => trait_path.push_str(tok),
                _ => {}
            }
            cursor_idx += 1;
        }
        let has_metavar_target = tokens.get(cursor_idx + 1).copied().is_some_and(|tok| {
            tok.strip_prefix('$').is_some_and(is_plain_ident)
                || (tok == "$"
                    && tokens
                        .get(cursor_idx + 2)
                        .copied()
                        .is_some_and(is_plain_ident))
        });
        if !aborted
            && tokens.get(cursor_idx).copied() == Some("for")
            && has_metavar_target
        {
            let clean = trait_path.trim();
            if !clean.is_empty()
                && !clean
                    .trim_start_matches("$crate::")
                    .contains(['$', '(', '{'])
                && !out.iter().any(|existing| existing == clean)
            {
                out.push(clean.to_owned());
            }
        }
        idx = cursor_idx.max(idx + 1);
    }
}

pub(super) fn extract_macro_impl_traits(tree: TsNode<'_>, source: &str, out: &mut Vec<String>) {
    push_transcription_impl_traits(tree, source, out);
}

fn extract_invocation_target_path(args_tree: TsNode<'_>, source: &str) -> Option<String> {
    let mut cursor = args_tree.walk();
    let mut path = String::new();
    let mut idx = 0usize;
    let children: Vec<_> = args_tree.children(&mut cursor).collect();
    while let Some(tok) = next_macro_token(&children, &mut idx, source) {
        if tok == "," || tok == ";" {
            break;
        }
        if matches!(tok, "::" | "crate" | "self" | "super")
            || (tok.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        {
            path.push_str(tok);
            idx += 1;
            continue;
        }
        break;
    }
    let last_seg = path.rsplit("::").next()?;
    last_seg
        .starts_with(|ch: char| ch.is_ascii_uppercase())
        .then_some(path)
}

pub(super) fn project_macro_invocation(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    let Some(owner) = facts
        .nodes
        .iter()
        .find(|node| node.id == ctx.owner)
        .filter(|node| matches!(node.kind.as_str(), "module" | "module_declaration"))
        .cloned()
    else {
        return;
    };
    let Some(macro_node) = ctx.node.child_by_field_name("macro") else {
        return;
    };
    let macro_raw = text(macro_node, ctx.source).trim();
    let macro_name = macro_raw.rsplit("::").next().unwrap_or(macro_raw);
    let template: Option<MacroTemplate> = FILE_MACRO_TEMPLATES.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|(_, map)| map.get(macro_name).cloned())
    });
    let Some(args_tree) = ctx
        .node
        .children(&mut ctx.node.walk())
        .find(|child| child.kind() == "token_tree")
    else {
        return;
    };
    let Some(template) = template else {
        if let Some(target_name) = extract_invocation_target_path(args_tree, ctx.source) {
            if let Some(owner_node) = facts.nodes.iter_mut().find(|node| node.id == ctx.owner) {
                if let Some(arr) = owner_node.details.as_object_mut().and_then(|obj| {
                    obj.entry("rust_macro_type_invocations")
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                }) {
                    arr.push(json!([macro_raw, target_name]));
                }
            }
        }
        return;
    };
    let Some((item_name, invocation_public, variants)) = extract_invocation_name_and_variants(
        args_tree,
        ctx.source,
        template.kind == MacroItemKind::Enum && template.has_variant_repetition,
    ) else {
        return;
    };
    let item_public = template.visibility.resolve(invocation_public);

    let item_qualname = format!("{}.{item_name}", owner.qualname);
    let item_kind = match template.kind {
        MacroItemKind::Struct => "struct",
        MacroItemKind::Enum => "enum",
        MacroItemKind::Function => "function",
    };
    if facts
        .nodes
        .iter()
        .any(|node| node.qualname == item_qualname && node.kind.as_str() == item_kind)
    {
        return;
    }

    let line = ctx.line();
    let end_line = ctx.node.end_position().row + ctx.offset + 1;
    let item_id = format!("rs:{}:{line}:{item_name}", owner.path);
    if facts.nodes.iter().any(|node| node.id == item_id) {
        return;
    }
    facts.nodes.push(Node {
        native_semantic: None,
        id: item_id.clone(),
        kind: crate::models::tags::NodeKind::parse(item_kind).expect("recognized macro item kind"),
        name: item_name.clone(),
        qualname: item_qualname.clone(),
        path: owner.path.clone(),
        line,
        end_line,
        is_test: owner.is_test,
        language: crate::models::tags::LanguageId::Rust,
        generated: true,
        details: json!({
            "is_public": item_public,
            "rust_macro_generated": true,
        }),
    });
    facts.edges.push(Edge {
        src: owner.id.clone(),
        dst: item_id.clone(),
        kind: "contains".into(),
        path: owner.path.clone(),
        line,
        evidence: "macro_rules".into(),
        confidence: "exact".into(),
    });

    if template.kind != MacroItemKind::Function {
        let synth_owner = SyntheticOwner {
            id: &item_id,
            qualname: &item_qualname,
            name: &item_name,
            path: &owner.path,
            line,
            end_line,
            is_public: item_public,
            is_test: owner.is_test,
        };
        for variant in variants {
            push_synthetic_constant(facts, &synth_owner, variant, item_public, true);
        }

        for c in &template.consts {
            push_synthetic_constant(
                facts,
                &synth_owner,
                &c.name,
                c.visibility.resolve(invocation_public),
                false,
            );
        }

        for method in &template.methods {
            push_synthetic_method(
                facts,
                &synth_owner,
                &method.name,
                method.is_static,
                method.visibility.resolve(invocation_public),
                method.synth_return,
                None,
            );
        }
        if template.has_from_str {
            push_synthetic_method(
                facts,
                &synth_owner,
                "from_str",
                true,
                item_public,
                Some("Result<Self>"),
                None,
            );
        }
        synthesize_derive_methods(&synth_owner, &template.derives, facts);
    }

    if let Some(macro_def) = facts
        .nodes
        .iter_mut()
        .find(|node| node.kind == crate::models::tags::NodeKind::Macro && node.name == template.name)
    {
        macro_def.set_detail(crate::models::details::NodeDetailKey::RUST_MACRO_PROJECTED, json!(true));
    }
}
