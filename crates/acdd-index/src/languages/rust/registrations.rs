use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::json;
use tree_sitter::Node as Syntax;

use crate::ast::relations;
use crate::ast::syntax::{field, text};
use crate::languages::context::LanguageContext;
use crate::languages::manifests::syntax::{AttributeRegistrationKind, SyntaxOperation};
use crate::languages::types::SyntaxContext;
use crate::models::{tags::LanguageId, tags::NodeKind, Edge, Facts, Node};
use crate::semantic::{ExtractionRuleOrigin, FrameworkFact, FrameworkScopeFacts, ValueFlowFacts};

const NS_MOD: u8 = 1;
const NS_MACRO: u8 = 2;
const NS_TYPE: u8 = 4;
const NS_VALUE: u8 = 8;

#[derive(Default)]
struct RegistrationScopeFrame {
    is_mod: bool,
    imports: HashMap<String, Vec<String>>,
    wildcards: Vec<String>,
    locals: HashMap<String, u8>,
    bindings: HashMap<String, Vec<usize>>,
}

enum PendingKind {
    Route {
        method: String,
        path: String,
        handler: Option<String>,
    },
    Named {
        cat: AttributeRegistrationKind,
        explicit: Option<String>,
    },
}

struct PendingReg {
    rule_idx: usize,
    row: usize,
    col: usize,
    kind: PendingKind,
}

struct PreparedState {
    rules: Arc<LanguageContext>,
    signals: BTreeSet<String>,
    pending: HashMap<usize, Vec<PendingReg>>,
}

thread_local! {
    static STATE: RefCell<Option<PreparedState>> = const { RefCell::new(None) };
}

pub(super) fn prepare(root: Syntax<'_>, source: &str, rules: &Arc<LanguageContext>) {
    let (mut attr_rules, mut builder_rules) = (Vec::new(), Vec::new());
    for (idx, rule) in rules.syntax_rules.iter().enumerate() {
        if rule.language != "rust" {
            continue;
        }
        match &rule.operation {
            SyntaxOperation::AttributeRegistration { .. } => attr_rules.push(idx),
            SyntaxOperation::RouteBuilderCall { .. } => builder_rules.push(idx),
            _ => {}
        }
    }
    let collect_signals =
        rules.has_syntax_gated_rules() && !rules.syntax_activation_keys().is_empty();
    if attr_rules.is_empty() && builder_rules.is_empty() && !collect_signals {
        STATE.with(|c| *c.borrow_mut() = None);
        return;
    }
    let mut a = FileAnalyzer {
        src: source,
        rules,
        attr_rules: &attr_rules,
        builder_rules: &builder_rules,
        collect_signals,
        frames: Vec::new(),
        signals: BTreeSet::new(),
        pending: HashMap::new(),
    };
    a.visit_container(root, true);
    STATE.with(|c| {
        *c.borrow_mut() = Some(PreparedState {
            rules: Arc::clone(rules),
            signals: a.signals,
            pending: a.pending,
        });
    });
}

pub(super) fn clear(facts: &mut Facts) {
    STATE.with(|c| {
        if let Some(s) = c.borrow_mut().take() {
            facts.activation_signals.syntax_attributes.extend(s.signals);
        }
    });
}

struct FileAnalyzer<'a> {
    src: &'a str,
    rules: &'a LanguageContext,
    attr_rules: &'a [usize],
    builder_rules: &'a [usize],
    collect_signals: bool,
    frames: Vec<RegistrationScopeFrame>,
    signals: BTreeSet<String>,
    pending: HashMap<usize, Vec<PendingReg>>,
}

impl<'a> FileAnalyzer<'a> {
    fn active_start(&self) -> usize {
        self.frames.iter().rposition(|f| f.is_mod).unwrap_or(0)
    }

    fn active(&self) -> &[RegistrationScopeFrame] {
        &self.frames[self.active_start()..]
    }

    fn populate_items(frame: &mut RegistrationScopeFrame, node: Syntax<'_>, src: &str) {
        let mut c = node.walk();
        for item in node.named_children(&mut c) {
            if item.kind() == "use_declaration" {
                let Some(arg) = item.child_by_field_name("argument") else {
                    continue;
                };
                let mut pairs = Vec::new();
                super::patterns::collect_use(arg, src, "", &mut pairs);
                for (full, alias) in pairs {
                    if alias == "*" {
                        if let Some(p) = full.strip_suffix("::*") {
                            frame.wildcards.push(p.to_owned());
                        }
                    } else {
                        frame.imports.entry(alias).or_default().push(full);
                    }
                }
                continue;
            }
            let Some(name) = field(item, src, "name") else {
                continue;
            };
            let bit = match item.kind() {
                "mod_item" => NS_MOD,
                "macro_definition" => NS_MACRO,
                "struct_item" | "enum_item" | "type_item" | "trait_item" => NS_TYPE,
                "function_item" | "const_item" | "static_item" => NS_VALUE,
                _ => continue,
            };
            *frame.locals.entry(name.to_owned()).or_default() |= bit;
        }
    }

    fn has_local(frame: &RegistrationScopeFrame, name: &str, bit: u8) -> bool {
        frame.locals.get(name).is_some_and(|m| m & bit != 0)
    }

    fn crate_shadowed(&self, path: &str) -> bool {
        path.split("::")
            .next()
            .is_some_and(|r| self.active().iter().any(|f| Self::has_local(f, r, NS_MOD)))
    }

    fn unique_import<'f>(frame: &'f RegistrationScopeFrame, alias: &str) -> Option<&'f str> {
        let targets = frame.imports.get(alias)?;
        let first = targets.first()?.as_str();
        targets.iter().all(|t| t == first).then_some(first)
    }

    fn match_path<'b>(
        &self,
        full: &str,
        mods: &[String],
        allowed: &impl Fn(&str) -> Option<&'b str>,
    ) -> Option<&'b str> {
        let (clean, abs) = full.strip_prefix("::").map_or((full, false), |s| (s, true));
        let (prefix, member) = clean.rsplit_once("::")?;
        (mods.iter().any(|m| m == prefix) && (abs || !self.crate_shadowed(clean)))
            .then(|| allowed(member))
            .flatten()
    }

    fn resolve_member<'b>(
        &self,
        raw: &str,
        mods: &[String],
        ns: u8,
        allowed: impl Fn(&str) -> Option<&'b str>,
    ) -> Option<&'b str> {
        let s = raw.trim();
        if s.starts_with("::") {
            return self.match_path(s, mods, &allowed);
        }
        if let Some((head, rest)) = s.split_once("::") {
            let mut base = None;
            for f in self.active().iter().rev() {
                if Self::has_local(f, head, NS_MOD) {
                    return None;
                }
                if f.imports.contains_key(head) {
                    let target = Self::unique_import(f, head)?;
                    if !target.starts_with("::") && self.crate_shadowed(target) {
                        return None;
                    }
                    base = Some(target);
                    break;
                }
            }
            let exp = base.map_or_else(|| s.to_owned(), |b| format!("{b}::{rest}"));
            return self.match_path(&exp, mods, &allowed);
        }
        let frames = self.active();
        for f in frames.iter().rev() {
            let shadowed =
                Self::has_local(f, s, ns) || (ns == NS_VALUE && f.bindings.contains_key(s));
            if shadowed {
                return None;
            }
            if f.imports.contains_key(s) {
                return self.match_path(Self::unique_import(f, s)?, mods, &allowed);
            }
        }
        let mut saw_wc = false;
        for f in frames.iter().rev().filter(|f| !f.wildcards.is_empty()) {
            saw_wc = true;
            let ok = f.wildcards.iter().all(|w| {
                let (c, abs) = w
                    .strip_prefix("::")
                    .map_or((w.as_str(), false), |x| (x, true));
                mods.iter().any(|m| m == c) && (abs || !self.crate_shadowed(c))
            });
            if !ok {
                return None;
            }
        }
        saw_wc.then(|| allowed(s)).flatten()
    }

    fn is_local_handler(&self, id: &str) -> bool {
        self.active().iter().enumerate().rev().any(|(i, f)| {
            f.bindings.contains_key(id) || (i > 0 && Self::has_local(f, id, NS_VALUE))
        })
    }

    fn lookup_binding(&self, id: &str) -> Vec<usize> {
        for f in self.active().iter().rev() {
            if let Some(r) = f.bindings.get(id) {
                return r.clone();
            }
            if Self::has_local(f, id, NS_VALUE) {
                return Vec::new();
            }
        }
        Vec::new()
    }

    fn filter_builders(&self, pred: impl Fn(&SyntaxOperation) -> bool) -> Vec<usize> {
        self.builder_rules
            .iter()
            .copied()
            .filter(|&i| pred(&self.rules.syntax_rules[i].operation))
            .collect()
    }

    fn builder_type_rules(&self, raw_type: &str, assoc_fn: Option<&str>) -> Vec<usize> {
        let clean = strip_type_wrappers(raw_type);
        self.filter_builders(|op| {
            let SyntaxOperation::RouteBuilderCall {
                module,
                builders,
                builder_constructors,
                ..
            } = op
            else {
                return false;
            };
            assoc_fn.is_none_or(|ctor| builder_constructors.iter().any(|c| c == ctor))
                && self
                    .resolve_member(clean, std::slice::from_ref(module), NS_TYPE, |b| {
                        builders
                            .iter()
                            .find(|x| x.as_str() == b)
                            .map(String::as_str)
                    })
                    .is_some()
        })
    }

    fn builder_factory_rules(&self, callee: &str) -> Vec<usize> {
        self.filter_builders(|op| {
            let SyntaxOperation::RouteBuilderCall {
                builder_factories, ..
            } = op
            else {
                return false;
            };
            builder_factories.iter().any(|fac| {
                fac.rsplit_once("::").is_some_and(|(m, f)| {
                    self.resolve_member(callee, &[m.to_owned()], NS_VALUE, |x| {
                        (x == f).then_some(f)
                    })
                    .is_some()
                })
            })
        })
    }

    fn expr_builder_rules(&self, node: Syntax<'_>) -> Vec<usize> {
        if node.kind() == "identifier" {
            return self.lookup_binding(text(node, self.src));
        }
        let Some(func) = (node.kind() == "call_expression")
            .then(|| node.child_by_field_name("function"))
            .flatten()
        else {
            return Vec::new();
        };
        match func.kind() {
            "field_expression" => {
                let (Some(recv), Some(fld)) = (
                    func.child_by_field_name("value"),
                    func.child_by_field_name("field"),
                ) else {
                    return Vec::new();
                };
                let method = text(fld, self.src);
                self.expr_builder_rules(recv)
                    .into_iter()
                    .filter(|&i| match &self.rules.syntax_rules[i].operation {
                        SyntaxOperation::RouteBuilderCall {
                            route_method,
                            chain_methods,
                            ..
                        } => method == route_method || chain_methods.iter().any(|c| c == method),
                        _ => false,
                    })
                    .collect()
            }
            "scoped_identifier" => {
                let raw = text(func, self.src);
                let mut r = raw.rsplit_once("::").map_or_else(Vec::new, |(ty, assoc)| {
                    self.builder_type_rules(ty, Some(assoc))
                });
                for idx in self.builder_factory_rules(raw) {
                    if !r.contains(&idx) {
                        r.push(idx);
                    }
                }
                r.sort_unstable();
                r
            }
            "identifier" => self.builder_factory_rules(text(func, self.src)),
            _ => Vec::new(),
        }
    }

    fn bind_pattern(&mut self, pat: Syntax<'_>, rules: Vec<usize>) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        let s = text(pat, self.src)
            .trim()
            .trim_start_matches("mut ")
            .trim_start_matches("ref ")
            .trim();
        if pat.kind() == "identifier"
            || (!s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
        {
            frame.bindings.insert(s.to_owned(), rules);
            return;
        }
        super::patterns::bindings(pat, |id| {
            frame
                .bindings
                .insert(text(id, self.src).to_owned(), Vec::new());
        });
    }

    fn check_func_attrs(&mut self, func_node: Syntax<'_>) {
        if self.attr_rules.is_empty() {
            return;
        }
        let mut out = Vec::new();
        super::macros::for_each_preceding_attribute(func_node, |item| {
            let Some(attr) = item
                .named_children(&mut item.walk())
                .find(|c| c.kind() == "attribute")
            else {
                return;
            };
            let Some(pn) = attr.named_child(0) else {
                return;
            };
            let attr_path = text(pn, self.src);
            let args = attr.child_by_field_name("arguments");
            for &idx in self.attr_rules {
                let SyntaxOperation::AttributeRegistration {
                    modules,
                    attributes,
                    category,
                    name_key,
                    path_argument,
                    method_key,
                    ..
                } = &self.rules.syntax_rules[idx].operation
                else {
                    continue;
                };
                let Some(val) = self.resolve_member(attr_path, modules, NS_MACRO, |m| {
                    attributes.get(m).map(String::as_str)
                }) else {
                    continue;
                };
                let (pos, named) = parse_attr_args(args, self.src);
                let kind = if *category == AttributeRegistrationKind::Route {
                    let Some(rp) =
                        path_argument.and_then(|i| pos.get(i).and_then(|v| v.as_deref()))
                    else {
                        continue;
                    };
                    let method = method_key
                        .as_deref()
                        .and_then(|k| named.get(k))
                        .map(|m| m.to_ascii_uppercase())
                        .unwrap_or_else(|| val.to_owned());
                    PendingKind::Route {
                        method,
                        path: rp.to_owned(),
                        handler: None,
                    }
                } else {
                    let explicit = name_key.as_deref().and_then(|k| named.get(k)).cloned();
                    PendingKind::Named {
                        cat: *category,
                        explicit,
                    }
                };
                let p = item.start_position();
                out.push(PendingReg {
                    rule_idx: idx,
                    row: p.row,
                    col: p.column,
                    kind,
                });
            }
        });
        if !out.is_empty() {
            self.pending
                .entry(func_node.start_byte())
                .or_default()
                .extend(out);
        }
    }

    fn handler_specs(
        &self,
        node: Syntax<'_>,
        mods: &[String],
        methods: &BTreeMap<String, String>,
        to_method: Option<&str>,
    ) -> Vec<(String, String)> {
        if node.kind() != "call_expression" {
            return Vec::new();
        }
        let (Some(func), Some(args)) = (
            node.child_by_field_name("function"),
            node.child_by_field_name("arguments"),
        ) else {
            return Vec::new();
        };
        let handler = args.named_child(0).and_then(|a| match a.kind() {
            "identifier" => {
                let n = text(a, self.src).trim();
                (!self.is_local_handler(n)).then_some(n)
            }
            "scoped_identifier" => Some(text(a, self.src).trim()),
            _ => None,
        });
        let resolve = |e: Syntax<'_>| {
            self.resolve_member(text(e, self.src), mods, NS_VALUE, |m| {
                methods.get(m).map(String::as_str)
            })
        };
        if let Some(to) = to_method {
            let ok = func.kind() == "field_expression"
                && func
                    .child_by_field_name("field")
                    .is_some_and(|f| text(f, self.src) == to);
            let Some(h) = handler.filter(|_| ok) else {
                return Vec::new();
            };
            let mut cur = func.child_by_field_name("value");
            while let Some(e) = cur.filter(|e| e.kind() == "call_expression") {
                let Some(inner) = e.child_by_field_name("function") else {
                    break;
                };
                if inner.kind() == "field_expression" {
                    cur = inner.child_by_field_name("value");
                    continue;
                }
                return resolve(inner)
                    .map(|m| vec![(m.to_owned(), h.to_owned())])
                    .unwrap_or_default();
            }
            return Vec::new();
        }
        match func.kind() {
            "identifier" | "scoped_identifier" => handler
                .and_then(|h| resolve(func).map(|m| vec![(m.to_owned(), h.to_owned())]))
                .unwrap_or_default(),
            "field_expression" => {
                let Some(recv) = func.child_by_field_name("value") else {
                    return Vec::new();
                };
                let mut out = self.handler_specs(recv, mods, methods, None);
                if !out.is_empty() {
                    if let (Some(f), Some(h)) = (func.child_by_field_name("field"), handler) {
                        if let Some(m) = methods.get(text(f, self.src)) {
                            out.push((m.clone(), h.to_owned()));
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn check_route_call(&mut self, call: Syntax<'_>) {
        if self.builder_rules.is_empty() {
            return;
        }
        let Some(func) = call
            .child_by_field_name("function")
            .filter(|f| f.kind() == "field_expression")
        else {
            return;
        };
        let (Some(recv), Some(mn), Some(an)) = (
            func.child_by_field_name("value"),
            func.child_by_field_name("field"),
            call.child_by_field_name("arguments"),
        ) else {
            return;
        };
        let recv_rules = self.expr_builder_rules(recv);
        if recv_rules.is_empty() {
            return;
        }
        let call_method = text(mn, self.src);
        let mut c = an.walk();
        let args: Vec<_> = an.named_children(&mut c).collect();
        let p = call.start_position();
        let mut out = Vec::new();
        for idx in recv_rules {
            let SyntaxOperation::RouteBuilderCall {
                route_method,
                path_argument,
                handler_argument,
                handler_modules,
                handler_methods,
                to_method,
                ..
            } = &self.rules.syntax_rules[idx].operation
            else {
                continue;
            };
            if call_method != route_method {
                continue;
            }
            let Some(rp) = args
                .get(*path_argument)
                .and_then(|n| super::unquote_rust_string(*n, self.src))
            else {
                continue;
            };
            let Some(hn) = args.get(*handler_argument) else {
                continue;
            };
            for (method, h) in
                self.handler_specs(*hn, handler_modules, handler_methods, to_method.as_deref())
            {
                out.push(PendingReg {
                    rule_idx: idx,
                    row: p.row,
                    col: p.column,
                    kind: PendingKind::Route {
                        method,
                        path: rp.as_ref().to_owned(),
                        handler: Some(h),
                    },
                });
            }
        }
        if !out.is_empty() {
            self.pending
                .entry(call.start_byte())
                .or_default()
                .extend(out);
        }
    }

    fn visit_container(&mut self, node: Syntax<'_>, is_mod: bool) {
        let mut frame = RegistrationScopeFrame {
            is_mod,
            ..Default::default()
        };
        Self::populate_items(&mut frame, node, self.src);
        self.frames.push(frame);
        self.visit_children(node);
        self.frames.pop();
    }

    fn visit_fields(&mut self, node: Syntax<'_>, fields: &[&str]) {
        for &f in fields {
            if let Some(child) = node.child_by_field_name(f) {
                self.visit_node(child);
            }
        }
    }

    fn visit_children(&mut self, node: Syntax<'_>) {
        let mut c = node.walk();
        for child in node.named_children(&mut c) {
            self.visit_node(child);
        }
    }

    fn visit_node(&mut self, node: Syntax<'_>) {
        match node.kind() {
            "attribute_item" if self.collect_signals => {
                if let Some(p) = node
                    .named_children(&mut node.walk())
                    .find(|c| c.kind() == "attribute")
                    .and_then(|a| a.named_child(0))
                    .map(|p| text(p, self.src))
                {
                    let keys = self.rules.syntax_activation_keys();
                    if keys.exact_attributes.contains(p)
                        || keys
                            .attribute_prefixes
                            .iter()
                            .any(|k| p.starts_with(k.as_str()))
                    {
                        self.signals.insert(p.to_owned());
                    }
                }
            }
            "mod_item" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit_container(body, true);
                }
            }
            "block" => self.visit_container(node, false),
            "if_expression" => {
                self.frames.push(RegistrationScopeFrame::default());
                self.visit_fields(node, &["condition", "consequence"]);
                self.frames.pop();
                self.visit_fields(node, &["alternative"]);
            }
            "while_expression" => {
                self.frames.push(RegistrationScopeFrame::default());
                self.visit_fields(node, &["condition", "body"]);
                self.frames.pop();
            }
            "for_expression" => {
                self.visit_fields(node, &["value"]);
                self.frames.push(RegistrationScopeFrame::default());
                if let Some(p) = node.child_by_field_name("pattern") {
                    self.bind_pattern(p, Vec::new());
                }
                self.visit_fields(node, &["body"]);
                self.frames.pop();
            }
            "function_item" => {
                self.check_func_attrs(node);
                let mut params = Vec::new();
                if let Some(plist) = node.child_by_field_name("parameters") {
                    let mut c = plist.walk();
                    for param in plist.named_children(&mut c) {
                        if let Some(pat) = param.child_by_field_name("pattern") {
                            let r = param
                                .child_by_field_name("type")
                                .map_or_else(Vec::new, |t| {
                                    self.builder_type_rules(text(t, self.src), None)
                                });
                            params.push((pat, r));
                        }
                    }
                }
                self.frames.push(RegistrationScopeFrame::default());
                for (pat, r) in params {
                    self.bind_pattern(pat, r);
                }
                if let Some(b) = node.child_by_field_name("body") {
                    self.visit_container(b, false);
                }
                self.frames.pop();
            }
            "closure_expression" | "match_arm" => {
                self.frames.push(RegistrationScopeFrame::default());
                for f in ["parameters", "pattern"] {
                    if let Some(p) = node.child_by_field_name(f) {
                        self.bind_pattern(p, Vec::new());
                    }
                }
                self.visit_children(node);
                self.frames.pop();
            }
            "let_condition" => {
                self.visit_fields(node, &["value"]);
                if let Some(p) = node.child_by_field_name("pattern") {
                    self.bind_pattern(p, Vec::new());
                }
            }
            "let_declaration" => {
                let mut rules = node
                    .child_by_field_name("value")
                    .map_or_else(Vec::new, |v| {
                        self.visit_node(v);
                        self.expr_builder_rules(v)
                    });
                if rules.is_empty() {
                    if let Some(ty) = node.child_by_field_name("type") {
                        rules = self.builder_type_rules(text(ty, self.src), None);
                    }
                }
                if let Some(p) = node.child_by_field_name("pattern") {
                    self.bind_pattern(p, rules);
                }
            }
            "assignment_expression" => {
                let rules = node
                    .child_by_field_name("right")
                    .map_or_else(Vec::new, |r| {
                        self.visit_node(r);
                        self.expr_builder_rules(r)
                    });
                if let Some(left) = node.child_by_field_name("left") {
                    self.visit_node(left);
                    if left.kind() == "identifier" {
                        let id = text(left, self.src).trim();
                        let start = self.active_start();
                        if let Some(slot) = self.frames[start..]
                            .iter_mut()
                            .rev()
                            .find_map(|f| f.bindings.get_mut(id))
                        {
                            *slot = rules;
                        }
                    }
                }
            }
            "call_expression" => {
                self.visit_children(node);
                self.check_route_call(node);
            }
            _ => self.visit_children(node),
        }
    }
}

fn strip_type_wrappers(raw: &str) -> &str {
    let mut s = raw.trim();
    while let Some(rest) = s.strip_prefix('&') {
        s = rest.trim_start();
        if let Some((_, next)) = s
            .strip_prefix('\'')
            .and_then(|a| a.split_once(char::is_whitespace))
        {
            s = next.trim_start();
        }
        if let Some(after) = s
            .strip_prefix("mut")
            .filter(|r| r.starts_with(char::is_whitespace))
        {
            s = after.trim_start();
        }
    }
    s.split('<').next().unwrap_or(s).trim()
}

fn parse_attr_args(
    args: Option<Syntax<'_>>,
    src: &str,
) -> (Vec<Option<String>>, BTreeMap<String, String>) {
    let (mut pos, mut named) = (Vec::new(), BTreeMap::new());
    let Some(tree) = args else {
        return (pos, named);
    };
    let mut slot = Vec::new();
    let flush = |slot: &mut Vec<Syntax<'_>>,
                 pos: &mut Vec<Option<String>>,
                 map: &mut BTreeMap<String, String>| {
        let is_str = |n: Syntax<'_>| matches!(n.kind(), "string_literal" | "raw_string_literal");
        if slot.len() >= 3
            && slot[0].kind() == "identifier"
            && text(slot[1], src) == "="
            && is_str(slot[2])
        {
            if let Some(v) = super::unquote_rust_string(slot[2], src) {
                map.insert(text(slot[0], src).to_owned(), v.into_owned());
            }
        } else if slot.len() == 1 && is_str(slot[0]) {
            pos.push(super::unquote_rust_string(slot[0], src).map(|v| v.into_owned()));
        } else if !slot.is_empty() {
            pos.push(None);
        }
        slot.clear();
    };
    let mut c = tree.walk();
    for ch in tree.children(&mut c) {
        match ch.kind() {
            "(" | ")" => {}
            "," => flush(&mut slot, &mut pos, &mut named),
            _ => slot.push(ch),
        }
    }
    flush(&mut slot, &mut pos, &mut named);
    (pos, named)
}

fn emit_pending(
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    func_owner: Option<(String, String, bool)>,
) {
    STATE.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(state) = borrow.as_mut() else { return };
        if !state.signals.is_empty() {
            facts
                .activation_signals
                .syntax_attributes
                .extend(std::mem::take(&mut state.signals));
        }
        let Some(list) = state.pending.get(&ctx.node.start_byte()) else { return };
        let Some(m) = facts.nodes.first() else { return };
        let (mod_id, mod_qual, path, mod_test) =
            (m.id.clone(), m.qualname.clone(), m.path.clone(), m.is_test);
        let is_test = func_owner
            .as_ref()
            .map_or_else(|| mod_test || super::has_cfg_test(ctx.node, ctx.source), |f| f.2);

        for reg in list {
            let rule = &state.rules.syntax_rules[reg.rule_idx];
            if !state.rules.is_syntax_rule_active(rule, &path, &facts.activation_signals) {
                continue;
            }
            let provider = match &rule.operation {
                SyntaxOperation::AttributeRegistration { provider, .. }
                | SyntaxOperation::RouteBuilderCall { provider, .. } => provider.as_str(),
                _ => continue,
            };
            let line = reg.row + ctx.offset + 1;
            let origin = ValueFlowFacts {
                framework: Some(Box::new(FrameworkScopeFacts {
                    facts: vec![FrameworkFact::RuleOrigin(ExtractionRuleOrigin {
                        pack: rule.pack.clone(),
                        rule: rule.id.clone(),
                    })],
                    ..Default::default()
                })),
                ..Default::default()
            };
            let (kind, id, name, qual, details, direct_dst, ref_expr, ev) = match &reg.kind {
                PendingKind::Route { method, path: rp, handler } => {
                    let h = handler
                        .as_deref()
                        .or_else(|| func_owner.as_ref().map(|f| f.0.as_str()));
                    let Some(h) = h else { continue };
                    let id = format!("route:{path}:{line}:{}:{method}", reg.col);
                    let name = format!("{method} {rp}");
                    let details = json!({
                        "method": method, "route_path": rp, "handler": h, "handlers": [h],
                        "framework": provider, "pack": rule.pack, "rule": rule.id, "value_flow": origin,
                    });
                    let direct = handler.is_none().then(|| h.to_owned());
                    let r = handler.as_deref();
                    (
                        NodeKind::Route,
                        id,
                        name.clone(),
                        format!("{mod_qual}.{name}"),
                        details,
                        direct,
                        r,
                        "configured route attribute",
                    )
                }
                PendingKind::Named { cat, explicit } => {
                    let Some((func_id, func_name, _)) = &func_owner else { continue };
                    let (kind, prefix, seg) = match cat {
                        AttributeRegistrationKind::Tool => {
                            (NodeKind::ToolRegistration, "tool-registration", "tool")
                        }
                        AttributeRegistrationKind::CliCommand => {
                            (NodeKind::CliCommandRegistration, "cli_command_registration", "cli")
                        }
                        _ => (
                            NodeKind::EventHandlerRegistration,
                            "event_handler_registration",
                            "event",
                        ),
                    };
                    let adv = explicit.as_deref().unwrap_or(func_name.as_str());
                    let id = format!("{prefix}:{path}:{line}:{func_name}");
                    let status = if explicit.is_some() { "explicit" } else { "default" };
                    let details = json!({
                        "framework": provider, "pack": rule.pack, "rule": rule.id,
                        "advertised_name": adv, "name_status": status, "handler": func_id,
                        "value_flow": origin,
                    });
                    (
                        kind,
                        id,
                        adv.to_owned(),
                        format!("{mod_qual}.{seg}.{adv}"),
                        details,
                        Some(func_id.clone()),
                        None,
                        "attributed function",
                    )
                }
            };
            if facts.nodes.iter().any(|n| n.id == id) {
                continue;
            }
            facts.pending_flow_nodes.push(facts.nodes.len());
            facts.nodes.push(Node {
                native_semantic: None,
                id: id.clone(),
                kind,
                name,
                qualname: qual,
                path: path.clone(),
                line,
                end_line: line,
                is_test,
                language: LanguageId::Rust,
                generated: false,
                details,
            });
            facts.edges.push(Edge {
                src: mod_id.clone(),
                dst: id.clone(),
                kind: "contains".into(),
                path: path.clone(),
                line,
                evidence: "configured rust registration".into(),
                confidence: "exact".into(),
            });
            if let Some(dst) = direct_dst {
                facts.edges.push(Edge {
                    src: id,
                    dst,
                    kind: "handles".into(),
                    path: path.clone(),
                    line,
                    evidence: ev.into(),
                    confidence: "exact".into(),
                });
            } else if let Some(expr) = ref_expr {
                relations::reference(facts, &id, expr, "handles", line);
            }
        }
    });
}

pub(super) fn extract_attribute_registrations(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    let func = facts
        .nodes
        .iter()
        .rev()
        .find(|n| n.id == ctx.owner)
        .map(|f| (f.id.clone(), f.name.clone(), f.is_test));
    if func.is_some() {
        emit_pending(ctx, facts, func);
    }
}

pub(super) fn extract_route_builder_calls(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    emit_pending(ctx, facts, None);
}
