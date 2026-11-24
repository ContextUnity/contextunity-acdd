use crate::ast::{
    relations,
    scope_analysis::ScopeAnalysis,
    syntax::{self, field, text, ScopeBindings, Syntax},
};
use crate::languages::context::{
    execute_call_rules, CallRulesInput, CallRulesResolvers, FileContext, LanguageContext,
};
use crate::languages::profile::LanguageProfile;
use crate::languages::routes::{self, RouteContext};
use crate::languages::sql;
use crate::languages::types::{
    call, module_stem, AstSearchCapabilities, ImportPath, LanguageFamily, LanguageFileState,
    PythonFileState, SymbolMetadata, SyntaxContext, SyntaxGrammarSpec,
};
use crate::models::{Facts, Node};
use std::collections::HashMap;
use std::sync::Arc;

/// Only summary-dependent, necessary candidates from the existing main walk.
pub(crate) struct DeferredEvent<'tree> {
    pub(crate) node: Syntax<'tree>,
    pub(crate) owner: String,
    pub(crate) frontiers: (usize, usize, usize),
    reference_frontier: usize,
    declaration: bool,
    registration_guards: Vec<Option<String>>,
    projection: bool,
}

pub(crate) fn prepare_deferred_event<'tree>(
    ctx: &SyntaxContext<'_, 'tree>,
    facts: &mut Facts,
    symbols: &HashMap<usize, String>,
    rules: &LanguageContext,
    rule_ir: &crate::languages::RuleIRHandle,
) -> Option<DeferredEvent<'tree>> {
    let kind = ctx.node.kind();
    if matches!(kind, "assignment" | "annotated_assignment") {
        syntax_operations::record_model_factory(ctx);
        syntax_operations::record_model_attribute(ctx, symbols, rules);
        syntax_operations::extract_class_attribute(
            ctx.node, ctx.source, ctx.owner, ctx.offset, facts, rules,
        );
    }
    let frontiers = (facts.nodes.len(), facts.edges.len(), facts.references.len());
    let declaration = matches!(kind, "function_definition" | "class_definition")
        && routes::declaration_candidate(ctx, rule_ir);
    let registration_guards = std::cell::RefCell::new(Vec::new());
    if kind == "call" {
        // This admission precedes prefix proof in the existing extractor. Keep
        // its incremental Facts cursor at the original event, then stop before
        // the unavailable completed-summary proof or any emitted route facts.
        routes::registration_with_context_filtered(
            ctx,
            facts,
            symbols,
            rule_ir,
            &routes::RouteContext::default(),
            |rule, facts| {
                if rule.registration_guard.as_deref().is_none_or(|guard| {
                    syntax_operations::is_receiver_registration(
                        ctx.node, ctx.source, symbols, facts, guard,
                    )
                }) {
                    registration_guards
                        .borrow_mut()
                        .push(rule.registration_guard.clone());
                }
                false
            },
        );
        call_to_file(ctx, facts, rules);
        routes::client_call(ctx, facts, rules, "python");
    }
    Python.extract_routes(ctx, facts, symbols);
    let projection = projections::candidate(ctx, rules);
    let registration_guards = registration_guards.into_inner();
    ((kind == "class_definition" && ctx.node.child_by_field_name("superclasses").is_some())
        || declaration
        || !registration_guards.is_empty()
        || projection)
        .then(|| DeferredEvent {
            node: ctx.node,
            owner: ctx.owner.to_owned(),
            frontiers,
            reference_frontier: facts.references.len(),
            declaration,
            registration_guards,
            projection,
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_deferred_event(
    event: &DeferredEvent<'_>,
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    symbols: &HashMap<usize, String>,
    rules: &LanguageContext,
    rule_ir: &crate::languages::RuleIRHandle,
    lexical: &routes::RouteContext,
    projections: &mut HashMap<String, crate::semantic::ValueFlowFacts>,
) {
    if ctx.node.kind() == "class_definition" {
        syntax_operations::extract_default_table(ctx, facts, rules, lexical);
    }
    if event.declaration {
        routes::declaration_with_context(ctx, facts, rule_ir, lexical);
    }
    if !event.registration_guards.is_empty() {
        routes::registration_with_context_filtered(
            ctx,
            facts,
            symbols,
            rule_ir,
            lexical,
            |rule, _| event.registration_guards.contains(&rule.registration_guard),
        );
    }
    if event.projection {
        projections::extract_with_frontier(
            ctx,
            facts,
            rules,
            lexical,
            symbols,
            event.reference_frontier,
            projections,
        );
    }
}
pub(crate) mod common;
mod computed_receivers;
mod decorated_registration;
mod lazy_exports;
pub(crate) mod lexical;
mod logging_factories;
mod module_exports;
mod projections;
mod syntax_operations;
mod template_loaders;

pub(crate) fn logging_factories(
    root: Syntax<'_>,
    source: &str,
    offset: usize,
) -> serde_json::Value {
    logging_factories::collect(root, source, offset)
}
pub(crate) mod dependencies;
pub(crate) mod value_flow;

use common::{direct_module_assignment, module_scope, type_alias};
pub(crate) use common::{kind, language};
pub(crate) use lexical::lexical_binding_owner;

fn class_method_owner(node: Syntax<'_>) -> Option<Syntax<'_>> {
    let mut parent = node.parent()?;
    if parent.kind() == "decorated_definition" {
        parent = parent.parent()?;
    }
    if parent.kind() != "block" {
        return None;
    }
    parent
        .parent()
        .filter(|owner| owner.kind() == "class_definition")
}

fn conditional_class_member(mut node: Syntax<'_>) -> bool {
    let mut conditional = false;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "class_definition" => return conditional,
            "function_definition" | "lambda" => return false,
            "if_statement" | "elif_clause" | "else_clause" | "for_statement"
            | "while_statement" | "try_statement" | "except_clause" | "finally_clause"
            | "with_statement" | "match_statement" | "case_clause" => conditional = true,
            _ => {}
        }
        node = parent;
    }
    false
}

fn is_static_decorator(decorator: &str) -> bool {
    let decorator = decorator
        .trim()
        .strip_prefix('@')
        .unwrap_or(decorator.trim());
    decorator
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .rsplit('.')
        .next()
        == Some("staticmethod")
}


fn is_inside_type_checking(
    mut curr: Syntax<'_>,
    source: &str,
    aliases: &HashMap<String, Vec<(usize, bool)>>,
) -> bool {
    while let Some(parent) = curr.parent() {
        if parent.kind() == "function_definition" || parent.kind() == "class_definition" {
            return false;
        }
        if parent.kind() == "if_statement" {
            let in_consequence = parent
                .child_by_field_name("consequence")
                .is_some_and(|cons| cons.id() == curr.id());
            if in_consequence {
                if let Some(cond) = parent.child_by_field_name("condition") {
                    let cond_text = text(cond, source).trim();
                    let clean = cond_text.split('(').next().unwrap_or(cond_text).trim();
                    if clean == "TYPE_CHECKING"
                        || clean == "typing.TYPE_CHECKING"
                        || clean == "typing_extensions.TYPE_CHECKING"
                        || clean.ends_with(".TYPE_CHECKING")
                        || is_type_checking_alias(aliases, parent.start_byte(), clean)
                    {
                        return true;
                    }
                }
            }
        }
        curr = parent;
    }
    false
}

/// `from typing import TYPE_CHECKING as TC` proves `if TC:` until that name is rebound.
fn is_type_checking_alias(
    aliases: &HashMap<String, Vec<(usize, bool)>>,
    position: usize,
    name: &str,
) -> bool {
    if name.is_empty() || name.contains('.') {
        return false;
    }
    let Some(events) = aliases.get(name) else {
        return false;
    };
    let prior = events.partition_point(|(event_position, _)| *event_position < position);
    prior > 0 && events[prior - 1].1
}

fn is_python_stub_body(node: Syntax<'_>, source: &str) -> bool {
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let mut non_trivial = Vec::new();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "comment" {
            continue;
        }
        if non_trivial.is_empty() && child.kind() == "expression_statement" {
            if let Some(first) = child.named_child(0) {
                if first.kind() == "string" {
                    continue;
                }
            }
        }
        non_trivial.push(child);
    }
    if non_trivial.is_empty() {
        return true;
    }
    non_trivial.iter().all(|stmt| {
        if stmt.kind() == "pass_statement" {
            return true;
        }
        if stmt.kind() == "expression_statement" {
            let t = text(*stmt, source).trim();
            if t == "..." || t == "Ellipsis" {
                return true;
            }
            if let Some(inner) = stmt.named_child(0) {
                if inner.kind() == "ellipsis" {
                    return true;
                }
            }
        }
        false
    })
}

struct Python;
static PYTHON: Python = Python;
static PYTHON_SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declarations: &[
        ("function_definition", "function"),
        ("lambda", "function"),
        ("class_definition", "class"),
        ("type_alias_statement", "type"),
        ("assignment", "variable"),
    ],
    references: &["call", "string"],
    scopes: &[
        "module",
        "function_definition",
        "lambda",
        "class_definition",
    ],
    imports: &["import_from_statement", "import_statement"],
    comments: &["comment"],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Python {
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&PYTHON_SPEC)
    }
    fn scope_analysis<'source, 'tree>(
        &self,
        node: Syntax<'tree>,
        source: &'source str,
        _file: &FileContext,
    ) -> Option<ScopeAnalysis<'source, 'tree>> {
        Some(ScopeAnalysis::python(node, source))
    }

    fn bindings(&self, node: Syntax<'_>, source: &str) -> ScopeBindings {
        crate::ast::syntax::bindings(node, source)
    }
    fn builtin_member(&self, receiver: &str, member: &str) -> bool {
        builtin_member(receiver, member)
    }
    fn builtin_generic(&self, receiver: &str) -> bool {
        matches!(
            receiver,
            "dict"
                | "Dict"
                | "list"
                | "List"
                | "set"
                | "Set"
                | "tuple"
                | "Tuple"
                | "Mapping"
                | "MutableMapping"
                | "Sequence"
                | "Iterable"
                | "type"
                | "Type"
        )
    }
    fn id(&self) -> &'static str {
        "python"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        language()
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("python")
    }
    fn symbol_kind(&self, k: &str) -> Option<&'static str> {
        match k {
            "assignment" => Some("variable"),
            _ => kind(k),
        }
    }
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        if node.kind() == "assignment" {
            None
        } else if node.kind() == "function_definition" && class_method_owner(node).is_some() {
            Some("method")
        } else {
            kind(node.kind())
        }
    }
    fn symbol_with_source(&self, node: Syntax<'_>, source: &str) -> Option<&'static str> {
        if matches!(node.kind(), "assignment" | "type_alias_statement") {
            if type_alias(node, source) {
                Some("type")
            } else {
                direct_module_assignment(node).then_some("variable")
            }
        } else {
            self.symbol(node)
        }
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        common::symbol_name(node, source)
    }
    fn node_prefix(&self, kind: &str) -> &'static str {
        match kind {
            "class" => "class",
            "type" => "type",
            _ => "py",
        }
    }
    fn module_name(&self, path: &str) -> String {
        module_stem(path)
            .trim_end_matches("/__init__")
            .replace('/', ".")
    }
    fn prepare(&self, root: Syntax<'_>, source: &str) -> FileContext {
        projections::clear();
        let mut type_checking_aliases = HashMap::new();
        module_exports::prepare(root, source, &mut type_checking_aliases);
        FileContext {
            state: LanguageFileState::Python(PythonFileState {
                type_checking_aliases,
            }),
            ..FileContext::default()
        }
    }
    fn prepare_with_context(
        &self,
        root: Syntax<'_>,
        source: &str,
        rules: &Arc<LanguageContext>,
    ) -> FileContext {
        syntax_operations::prepare_file(root, source, rules);
        let mut file = self.prepare(root, source);
        file.rules = Arc::clone(rules);
        file
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let (node, source) = (ctx.node, ctx.source);
        if !matches!(node.kind(), "import_from_statement" | "import_statement") {
            return;
        }
        projections::observe_import(ctx);
        let statement = text(node, source);
        let is_type_only = ctx
            .file_state
            .python()
            .is_some_and(|state| is_inside_type_checking(node, source, &state.type_checking_aliases));
        let mut add = |expression, alias, module| {
            ctx.import(facts, expression, alias, module);
            if is_type_only {
                if let Some(reference) = facts.references.last_mut() {
                    reference.receiver_hint =
                        Some(crate::models::ReceiverHint::TypeOnlyImport);
                }
            }
        };
        match node.kind() {
            "import_from_statement" => {
                let module = field(node, source, "module_name").unwrap_or("").to_owned();
                let mut c = node.walk();
                for child in node.children_by_field_name("name", &mut c) {
                    let name = field(child, source, "name").unwrap_or_else(|| text(child, source));
                    add(
                        name.into(),
                        field(child, source, "alias")
                            .map(str::to_owned)
                            .or_else(|| Some(name.into())),
                        Some(module.clone()),
                    );
                }
                if statement.contains('*') {
                    add("*".into(), Some("*".into()), Some(module));
                }
            }
            "import_statement"
                if statement.starts_with("import ")
                    && !statement.contains("from ")
                    && !statement.contains('"')
                    && !statement.contains('\'') =>
            {
                let mut c = node.walk();
                for child in node.children_by_field_name("name", &mut c) {
                    let name = field(child, source, "name").unwrap_or_else(|| text(child, source));
                    add(
                        name.into(),
                        field(child, source, "alias")
                            .map(str::to_owned)
                            .or_else(|| Some(name.split('.').next().unwrap_or(name).into())),
                        Some(name.into()),
                    );
                }
            }
            _ => {}
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        sql::extract_literal(ctx, facts);
        if matches!(ctx.node.kind(), "call") {
            call(ctx, facts, false);
            template_loaders::extract(ctx, facts, |alias, visit| {
                syntax_operations::for_each_loader_rule(alias, visit)
            });
            syntax_operations::extract_reverse_route(
                ctx.node, ctx.source, ctx.owner, ctx.offset, facts,
            );
            if let Some(hint) = computed_receivers::receiver_hint(ctx.node, ctx.source) {
                if let Some(reference) = facts.references.last_mut() {
                    reference.receiver_hint = Some(hint);
                }
            }
        }
    }
    fn external_import(&self, module: &str) -> Option<&'static str> {
        let root = module.split('.').next().unwrap_or(module);
        if is_python_stdlib(root) {
            Some("Python standard library")
        } else {
            None
        }
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &[
            "pyproject.toml",
            "requirements*.txt",
            "setup.cfg",
            "Pipfile",
        ]
    }
    fn extract_manifest_dependencies(&self, filename: &str, content: &str) -> Vec<String> {
        dependencies::dependencies(filename, content)
    }
    fn is_stdlib(&self, module: &str) -> bool {
        is_python_stdlib(module.split('.').next().unwrap_or(module))
    }
    fn builtin(&self, symbol: &str) -> bool {
        matches!(
            symbol,
            "print"
                | "len"
                | "isinstance"
                | "issubclass"
                | "str"
                | "int"
                | "float"
                | "bool"
                | "list"
                | "dict"
                | "set"
                | "tuple"
                | "super"
                | "getattr"
                | "globals"
                | "setattr"
                | "hasattr"
                | "delattr"
                | "locals"
                | "type"
                | "repr"
                | "open"
                | "iter"
                | "next"
                | "any"
                | "all"
                | "min"
                | "max"
                | "sum"
                | "enumerate"
                | "zip"
                | "sorted"
                | "reversed"
                | "abs"
                | "round"
                | "id"
                | "input"
                | "hash"
                | "callable"
                | "dir"
                | "vars"
                | "help"
                | "range"
                | "frozenset"
                | "bytes"
                | "bytearray"
                | "memoryview"
                | "complex"
                | "slice"
                | "object"
                | "classmethod"
                | "staticmethod"
                | "property"
                | "filter"
                | "map"
                | "format"
                | "pow"
                | "divmod"
                | "bin"
                | "hex"
                | "oct"
                | "ord"
                | "chr"
                | "breakpoint"
                | "compile"
                | "eval"
                | "exec"
                | "BaseException"
                | "Exception"
                | "ArithmeticError"
                | "BufferError"
                | "LookupError"
                | "AssertionError"
                | "AttributeError"
                | "EOFError"
                | "FloatingPointError"
                | "GeneratorExit"
                | "ImportError"
                | "ModuleNotFoundError"
                | "IndexError"
                | "KeyError"
                | "KeyboardInterrupt"
                | "MemoryError"
                | "NameError"
                | "NotImplementedError"
                | "OSError"
                | "OverflowError"
                | "RecursionError"
                | "ReferenceError"
                | "RuntimeError"
                | "StopIteration"
                | "StopAsyncIteration"
                | "SyntaxError"
                | "IndentationError"
                | "TabError"
                | "SystemError"
                | "SystemExit"
                | "TypeError"
                | "UnboundLocalError"
                | "UnicodeError"
                | "UnicodeEncodeError"
                | "UnicodeDecodeError"
                | "UnicodeTranslateError"
                | "ValueError"
                | "ZeroDivisionError"
                | "EnvironmentError"
                | "IOError"
                | "BlockingIOError"
                | "ChildProcessError"
                | "ConnectionError"
                | "BrokenPipeError"
                | "ConnectionAbortedError"
                | "ConnectionRefusedError"
                | "ConnectionResetError"
                | "FileExistsError"
                | "FileNotFoundError"
                | "InterruptedError"
                | "IsADirectoryError"
                | "NotADirectoryError"
                | "PermissionError"
                | "ProcessLookupError"
                | "TimeoutError"
                | "Warning"
                | "UserWarning"
                | "DeprecationWarning"
                | "PendingDeprecationWarning"
                | "SyntaxWarning"
                | "RuntimeWarning"
                | "FutureWarning"
                | "ImportWarning"
                | "UnicodeWarning"
                | "BytesWarning"
                | "ResourceWarning"
                | "Any"
                | "Optional"
                | "Union"
                | "List"
                | "Dict"
                | "Set"
                | "Tuple"
                | "Callable"
                | "Iterator"
                | "Generator"
                | "Sequence"
                | "Mapping"
                | "Literal"
                | "TypeVar"
                | "Generic"
                | "Annotated"
                | "Protocol"
        )
    }
    fn builtin_type(&self, name: &str) -> bool {
        matches!(
            name,
            "str"
                | "int"
                | "float"
                | "bool"
                | "bytes"
                | "bytearray"
                | "dict"
                | "list"
                | "set"
                | "tuple"
        )
    }

    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        common::normalize_python_import(owner, module)
    }
    fn doc_comment(&self, node: Syntax<'_>, source: &str) -> String {
        if let Some(first) = node
            .child_by_field_name("body")
            .and_then(|b| b.named_child(0))
        {
            if first.kind() == "expression_statement"
                && first.named_child(0).is_some_and(|n| n.kind() == "string")
            {
                return text(first, source).into();
            }
        }
        syntax::doc_comment(node, source)
    }
    fn receiver(&self, name: &str, owner: &Node) -> bool {
        matches!(name, "self" | "cls") && *owner.detail_entry(crate::models::details::NodeDetailKey::RECEIVER_NAME) == name
    }
    fn value_flow(&self, node: Syntax<'_>, source: &str) -> crate::semantic::ValueFlowFacts {
        value_flow::extract(node, source)
    }
    fn value_flow_with_context(
        &self,
        node: Syntax<'_>,
        source: &str,
        _file: &FileContext,
    ) -> crate::semantic::ValueFlowFacts {
        value_flow::extract(node, source)
    }
    fn metadata(
        &self,
        node: Syntax<'_>,
        source: &str,
        _name: &str,
        file: &FileContext,
    ) -> SymbolMetadata {
        let decorators = node
            .parent()
            .filter(|p| p.kind() == "decorated_definition")
            .map(|p| {
                let mut c = p.walk();
                p.named_children(&mut c)
                    .filter(|n| n.kind() == "decorator")
                    .map(|n| text(n, source).to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let class_owner = class_method_owner(node);
        let receiver_name = if class_owner.is_some()
            && !decorators
                .iter()
                .any(|decorator| is_static_decorator(decorator))
        {
            node.child_by_field_name("parameters")
                .and_then(|p| p.named_child(0))
                .and_then(|p| {
                    if p.kind() == "identifier" {
                        Some(text(p, source))
                    } else {
                        p.named_child(0)
                            .filter(|n| n.kind() == "identifier")
                            .map(|n| text(n, source))
                    }
                })
                .map(str::to_owned)
                .filter(|name| matches!(name.as_str(), "self" | "cls"))
        } else {
            None
        };
        let receiver_type = class_owner
            .and_then(|owner| field(owner, source, "name"))
            .map(str::to_owned);
        let is_method = class_owner.is_some().then_some(true);
        let is_static = class_owner.is_some().then_some(receiver_name.is_none());
        let mut param_types = std::collections::BTreeMap::new();
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if !matches!(
                    parameter.kind(),
                    "typed_parameter" | "typed_default_parameter"
                ) {
                    continue;
                }
                let Some(annotation) = field(parameter, source, "type") else {
                    continue;
                };
                let Some(name) = parameter
                    .child_by_field_name("name")
                    .or_else(|| parameter.named_child(0))
                else {
                    continue;
                };
                if name.kind() != "identifier" {
                    continue;
                }
                let annotation = annotation.trim();
                let annotation = annotation
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
                    .or_else(|| {
                        annotation
                            .strip_prefix('"')
                            .and_then(|value| value.strip_suffix('"'))
                    })
                    .unwrap_or(annotation);
                param_types.insert(text(name, source).to_owned(), annotation.to_owned());
            }
        }
        let is_overload = decorators.iter().any(|decorator| {
            let d = decorator.trim_start_matches('@').trim();
            let name = d.split('(').next().unwrap_or(d).trim();
            name == "overload"
                || name == "typing.overload"
                || name == "typing_extensions.overload"
                || name.ends_with(".overload")
        });
        let is_stub = is_overload
            || file.state.python().is_some_and(|state| {
                is_inside_type_checking(node, source, &state.type_checking_aliases)
            })
            || (node.kind() == "function_definition" && is_python_stub_body(node, source));
        SymbolMetadata {
            receiver_name,
            decorators,
            bases: field(node, source, "superclasses").map(str::to_owned),
            receiver_type,
            is_method,
            is_static,
            param_types,
            is_async: text(node, source).trim_start().starts_with("async "),
            is_overload,
            is_stub,
            is_conditional: conditional_class_member(node),
            is_public: Some(module_scope(node) && !_name.starts_with('_')),
            ..Default::default()
        }
    }
    fn test_role(
        &self,
        node: Syntax<'_>,
        source: &str,
        path: &str,
        _file: &FileContext,
    ) -> Option<&'static str> {
        if !crate::models::is_test(path)
            || !matches!(node.kind(), "function_definition" | "lambda")
        {
            return None;
        }
        let name = field(node, source, "name").unwrap_or_default();
        if name.starts_with("test_") {
            return Some("test_case");
        }
        let decorators = node
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")
            .map(|parent| {
                let mut cursor = parent.walk();
                parent
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() == "decorator")
                    .map(|decorator| text(decorator, source).trim().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if decorators.iter().any(|decorator| {
            let name = decorator
                .trim_start_matches('@')
                .split('(')
                .next()
                .unwrap_or_default()
                .trim();
            name == "fixture" || name.ends_with(".fixture")
        }) {
            return Some("test_fixture");
        }
        Some("test_helper")
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if let Some(bases) = ctx.node.child_by_field_name("superclasses") {
            let mut c = bases.walk();
            for base in bases
                .named_children(&mut c)
                .filter(|n| n.kind() != "keyword_argument")
            {
                relations::reference(
                    facts,
                    ctx.owner,
                    relations::type_name(base, ctx.source),
                    "inherits",
                    ctx.line(),
                );
            }
        }
        if ctx.node.kind() == "function_definition" {
            if let Some(parameters) = ctx.node.child_by_field_name("parameters") {
                let mut c = parameters.walk();
                for param in parameters.named_children(&mut c) {
                    if let Some(ty) = param.child_by_field_name("type") {
                        relations::type_references_at(facts, ctx.owner, ty, ctx.source, ctx.offset);
                    }
                }
            }
            if let Some(ret) = ctx.node.child_by_field_name("return_type") {
                relations::type_references_at(facts, ctx.owner, ret, ctx.source, ctx.offset);
            }
        }
        let span = relations::RelationSpan {
            node: ctx.node,
            source: ctx.source,
            owner: ctx.owner,
            offset: ctx.offset,
        };
        relations::decorator_references(span, facts);
        decorated_registration::decorator_candidates(ctx, facts);
    }
    fn extract_mutations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if !matches!(
            ctx.node.kind(),
            "assignment" | "augmented_assignment" | "attribute"
        ) {
            return;
        }
        let span = relations::RelationSpan {
            node: ctx.node,
            source: ctx.source,
            owner: ctx.owner,
            offset: ctx.offset,
        };
        relations::mutation(
            span,
            facts,
            &[],
            &["assignment", "augmented_assignment"],
            &["attribute"],
        );
        relations::member_access(span, facts, &["attribute"]);
    }
    fn extract_routes(
        &self,
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        _symbols: &HashMap<usize, String>,
    ) {
        if ctx.node.kind() != "assignment"
            || !ctx
                .node
                .child_by_field_name("left")
                .is_some_and(|left| left.kind() == "identifier")
            || !ctx
                .node
                .parent()
                .and_then(|node| node.parent())
                .is_some_and(|node| node.kind() == "module")
        {
            return;
        }
        if !ctx.owner.starts_with("module:") {
            let module_id = facts
                .nodes
                .first()
                .filter(|node| node.kind == crate::models::tags::NodeKind::Module)
                .map(|node| node.id.clone())
                .or_else(|| {
                    facts
                        .nodes
                        .iter()
                        .find(|node| node.kind == crate::models::tags::NodeKind::Module)
                        .map(|node| node.id.clone())
                });
            if let Some(module_id) = module_id {
                let module_ctx = SyntaxContext {
                    node: ctx.node,
                    source: ctx.source,
                    owner: &module_id,
                    offset: ctx.offset,
                    file_state: ctx.file_state,
                };
                decorated_registration::module_assignment(&module_ctx, facts);
            }
        } else {
            decorated_registration::module_assignment(ctx, facts);
        }
    }
    fn typed_scope_projections(&self) -> bool {
        true
    }
    fn extract_routes_with_typed_projections(
        &self,
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        symbols: &HashMap<usize, String>,
        rules: &LanguageContext,
        rule_ir: &crate::languages::RuleIRHandle,
        route_context: &RouteContext,
        projections: &mut HashMap<String, crate::semantic::ValueFlowFacts>,
    ) {
        if ctx.node.kind() == "class_definition" && !route_context.defer_python_orm() {
            syntax_operations::extract_default_table(ctx, facts, rules, route_context);
        }
        if matches!(ctx.node.kind(), "assignment" | "annotated_assignment") {
            syntax_operations::record_model_factory(ctx);
            syntax_operations::record_model_attribute(ctx, symbols, rules);
            syntax_operations::extract_class_attribute(
                ctx.node, ctx.source, ctx.owner, ctx.offset, facts, rules,
            );
        }
        if matches!(ctx.node.kind(), "function_definition" | "class_definition") {
            routes::declaration_with_context(ctx, facts, rule_ir, route_context);
        }
        if ctx.node.kind() == "call" {
            routes::registration_with_context_filtered(
                ctx,
                facts,
                symbols,
                rule_ir,
                route_context,
                |rule, facts| {
                    rule.registration_guard.as_deref().is_none_or(|guard| {
                        syntax_operations::is_receiver_registration(
                            ctx.node, ctx.source, symbols, facts, guard,
                        )
                    })
                },
            );
            call_to_file(ctx, facts, rules);
            routes::client_call(ctx, facts, rules, self.id());
        }
        self.extract_routes(ctx, facts, symbols);
        let reference_frontier = facts.references.len();
        projections::extract_with_frontier(
            ctx,
            facts,
            rules,
            route_context,
            symbols,
            reference_frontier,
            projections,
        );
    }
    fn finish(&self, facts: &mut Facts) {
        projections::clear();
        relations::implicit_fields(facts);
        decorated_registration::registrations(facts);
        syntax_operations::clear_file_context();
    }
    fn ast_search_capabilities(&self) -> AstSearchCapabilities {
        AstSearchCapabilities {
            declaration_kinds: &["function_definition", "class_definition"],
            declaration_body_kinds: &["block"],
            declaration_completions: &[":\n    __ACDD_META_BODY\n", "\n    __ACDD_META_BODY\n"],
            attribute_kinds: &["decorator"],
            valid_examples: &[
                "class Example",
                "class Example:",
                "def example($$$)",
                "def example():\n    pass",
                "client.fetch($VALUE)",
                "@property\ndef value(self):\n    return 1",
            ],
            diagnostic_hint: "Use a complete Python statement or expression, or a supported bodyless function or class declaration (e.g. 'class Foo', 'class Foo:', or 'def bar($$$):').",
            ..AstSearchCapabilities::default()
        }
    }
}

pub(crate) fn finish_class_method_registrations(facts: &mut Facts, lexical: &RouteContext) {
    syntax_operations::extract_class_method_registrations(facts, lexical);
}

pub(crate) fn has_class_registration_import() -> bool {
    syntax_operations::has_class_registration_import()
}

pub(crate) fn finish_default_table(
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    rules: &LanguageContext,
    lexical: &routes::RouteContext,
) {
    syntax_operations::extract_default_table(ctx, facts, rules, lexical);
}

fn call_to_file(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts, rules: &LanguageContext) {
    let indices = rules
        .prepared_projections("python")
        .map_or(&[][..], |prepared| prepared.call_to_file.as_slice());
    if indices.is_empty() {
        return;
    }
    let Some(callee) = ctx
        .node
        .child_by_field_name("function")
        .filter(|node| node.kind() == "identifier")
    else {
        return;
    };
    let alias = text(callee, ctx.source);
    execute_call_rules(
        CallRulesInput {
            ctx,
            rules,
            indices,
            language: "python",
            alias,
        },
        &CallRulesResolvers {
            candidate: &|module| projections::imported_module_candidate(alias, module),
            argument_value: &|index| {
                let arguments = ctx.node.child_by_field_name("arguments")?;
                let mut cursor = arguments.walk();
                let mut positional_index = 0;
                let mut target = None;
                let mut keyword_seen = false;
                for argument in arguments.named_children(&mut cursor) {
                    match argument.kind() {
                        "comment" => continue,
                        "keyword_argument" => keyword_seen = true,
                        "list_splat" | "dictionary_splat" => return None,
                        _ => {
                            if keyword_seen {
                                return None;
                            }
                            if positional_index == index {
                                target = Some(argument);
                            }
                            positional_index += 1;
                        }
                    }
                }
                target.and_then(|node| template_loaders::literal(node, ctx.source))
            },
            imported: &|module, export| {
                let Some(scope) = lexical_binding_owner(ctx.node, ctx.source, alias) else {
                    return false;
                };
                let root = match scope.kind() {
                    "module" => scope,
                    "function_definition" => {
                        let Some(body) = scope.child_by_field_name("body") else {
                            return false;
                        };
                        body
                    }
                    _ => return false,
                };
                template_loaders::imported(
                    root,
                    ctx.source,
                    alias,
                    module,
                    export,
                    ctx.node.start_byte(),
                )
            },
        },
        facts,
    );
}

fn is_text_bytes_common_member(member: &str) -> bool {
    matches!(
        member,
        "capitalize"
            | "center"
            | "count"
            | "endswith"
            | "expandtabs"
            | "find"
            | "index"
            | "isalnum"
            | "isalpha"
            | "isascii"
            | "isdigit"
            | "islower"
            | "isspace"
            | "istitle"
            | "isupper"
            | "join"
            | "ljust"
            | "lower"
            | "lstrip"
            | "maketrans"
            | "partition"
            | "removeprefix"
            | "removesuffix"
            | "replace"
            | "rfind"
            | "rindex"
            | "rjust"
            | "rpartition"
            | "rsplit"
            | "rstrip"
            | "split"
            | "splitlines"
            | "startswith"
            | "strip"
            | "swapcase"
            | "title"
            | "translate"
            | "upper"
            | "zfill"
    )
}

fn is_logger_member(member: &str) -> bool {
    matches!(
        member,
        "info"
            | "warning"
            | "warn"
            | "error"
            | "debug"
            | "critical"
            | "fatal"
            | "exception"
            | "log"
            | "isEnabledFor"
            | "getEffectiveLevel"
            | "hasHandlers"
            | "getChild"
            | "setLevel"
            | "addHandler"
            | "removeHandler"
    )
}

pub(crate) fn builtin_member(receiver: &str, member: &str) -> bool {
    match receiver {
        "str" => {
            is_text_bytes_common_member(member)
                || matches!(
                    member,
                    "casefold"
                        | "encode"
                        | "format"
                        | "format_map"
                        | "isdecimal"
                        | "isidentifier"
                        | "isnumeric"
                        | "isprintable"
                )
        }
        "bytes" => {
            is_text_bytes_common_member(member) || matches!(member, "decode" | "fromhex" | "hex")
        }
        "bytearray" => {
            builtin_member("bytes", member)
                || matches!(
                    member,
                    "append" | "clear" | "copy" | "extend" | "insert" | "pop" | "remove" | "reverse"
                )
        }
        "list" => matches!(
            member,
            "append"
                | "clear"
                | "copy"
                | "count"
                | "extend"
                | "index"
                | "insert"
                | "pop"
                | "remove"
                | "reverse"
                | "sort"
        ),
        "dict" | "Dict" | "MutableMapping" => matches!(
            member,
            "clear"
                | "copy"
                | "fromkeys"
                | "get"
                | "items"
                | "keys"
                | "pop"
                | "popitem"
                | "setdefault"
                | "update"
                | "values"
        ),
        "Mapping" => matches!(member, "get" | "items" | "keys" | "values"),
        "pathlib.PurePath" | "pathlib.PurePosixPath" | "pathlib.PureWindowsPath" => matches!(
            member,
            "as_posix"
                | "as_uri"
                | "is_absolute"
                | "is_relative_to"
                | "is_reserved"
                | "joinpath"
                | "match"
                | "relative_to"
                | "with_name"
                | "with_stem"
                | "with_suffix"
        ),
        "pathlib.Path" | "pathlib.PosixPath" | "pathlib.WindowsPath" => {
            builtin_member("pathlib.PurePath", member)
                || matches!(
                    member,
                    "absolute"
                        | "chmod"
                        | "exists"
                        | "expanduser"
                        | "glob"
                        | "hardlink_to"
                        | "is_block_device"
                        | "is_char_device"
                        | "is_dir"
                        | "is_fifo"
                        | "is_file"
                        | "is_mount"
                        | "is_socket"
                        | "is_symlink"
                        | "iterdir"
                        | "lstat"
                        | "mkdir"
                        | "open"
                        | "read_bytes"
                        | "read_text"
                        | "readlink"
                        | "rename"
                        | "replace"
                        | "resolve"
                        | "rglob"
                        | "rmdir"
                        | "samefile"
                        | "stat"
                        | "symlink_to"
                        | "touch"
                        | "unlink"
                        | "write_bytes"
                        | "write_text"
                )
        }
        "Logger" | "logging.Logger" => is_logger_member(member),
        "LoggerAdapter" | "logging.LoggerAdapter" => {
            is_logger_member(member) || member == "process"
        }
        "sqlite3.Connection" => matches!(
            member,
            "execute"
                | "executemany"
                | "executescript"
                | "cursor"
                | "commit"
                | "rollback"
                | "close"
                | "total_changes"
                | "interrupt"
        ),
        "sqlite3.Cursor" => matches!(
            member,
            "execute"
                | "executemany"
                | "executescript"
                | "fetchone"
                | "fetchmany"
                | "fetchall"
                | "close"
        ),
        "set" => matches!(
            member,
            "add"
                | "clear"
                | "copy"
                | "difference"
                | "difference_update"
                | "discard"
                | "intersection"
                | "intersection_update"
                | "isdisjoint"
                | "issubset"
                | "issuperset"
                | "pop"
                | "remove"
                | "symmetric_difference"
                | "symmetric_difference_update"
                | "union"
                | "update"
        ),
        "frozenset" => matches!(
            member,
            "copy"
                | "difference"
                | "intersection"
                | "isdisjoint"
                | "issubset"
                | "issuperset"
                | "symmetric_difference"
                | "union"
        ),
        "tuple" => matches!(member, "count" | "index"),
        "int" => matches!(
            member,
            "as_integer_ratio"
                | "bit_count"
                | "bit_length"
                | "conjugate"
                | "from_bytes"
                | "to_bytes"
        ),
        "float" => matches!(
            member,
            "as_integer_ratio" | "conjugate" | "fromhex" | "hex" | "is_integer"
        ),
        _ => false,
    }
}

fn is_python_stdlib(pkg: &str) -> bool {
    matches!(
        pkg,
        "__future__"
            | "abc"
            | "aifc"
            | "antigravity"
            | "argparse"
            | "array"
            | "ast"
            | "asynchat"
            | "asyncio"
            | "asyncore"
            | "atexit"
            | "audioop"
            | "base64"
            | "bdb"
            | "binascii"
            | "binhex"
            | "bisect"
            | "builtins"
            | "bz2"
            | "calendar"
            | "cgi"
            | "cgitb"
            | "chunk"
            | "cmath"
            | "cmd"
            | "code"
            | "codecs"
            | "codeop"
            | "collections"
            | "colorsys"
            | "compileall"
            | "concurrent"
            | "configparser"
            | "contextlib"
            | "contextvars"
            | "copy"
            | "copyreg"
            | "cProfile"
            | "crypt"
            | "csv"
            | "ctypes"
            | "curses"
            | "dataclasses"
            | "datetime"
            | "dbm"
            | "decimal"
            | "difflib"
            | "dis"
            | "distutils"
            | "doctest"
            | "email"
            | "encodings"
            | "enum"
            | "errno"
            | "faulthandler"
            | "fcntl"
            | "filecmp"
            | "fileinput"
            | "fnmatch"
            | "fractions"
            | "ftplib"
            | "functools"
            | "gc"
            | "getopt"
            | "getpass"
            | "gettext"
            | "glob"
            | "graphlib"
            | "grp"
            | "gzip"
            | "hashlib"
            | "heapq"
            | "hmac"
            | "html"
            | "http"
            | "idlelib"
            | "imaplib"
            | "imghdr"
            | "imp"
            | "importlib"
            | "inspect"
            | "io"
            | "ipaddress"
            | "itertools"
            | "json"
            | "keyword"
            | "lib2to3"
            | "linecache"
            | "locale"
            | "logging"
            | "lzma"
            | "mailbox"
            | "mailcap"
            | "marshal"
            | "math"
            | "mimetypes"
            | "mmap"
            | "modulefinder"
            | "msilib"
            | "msvcrt"
            | "multiprocessing"
            | "netrc"
            | "nis"
            | "nntplib"
            | "numbers"
            | "operator"
            | "optparse"
            | "os"
            | "ossaudiodev"
            | "parser"
            | "pathlib"
            | "pdb"
            | "pickle"
            | "pickletools"
            | "pipes"
            | "pkgutil"
            | "platform"
            | "plistlib"
            | "poplib"
            | "posix"
            | "posixpath"
            | "pprint"
            | "profile"
            | "pstats"
            | "pty"
            | "pwd"
            | "py_compile"
            | "pyclbr"
            | "pydoc"
            | "queue"
            | "quopri"
            | "random"
            | "re"
            | "readline"
            | "reprlib"
            | "resource"
            | "rlcompleter"
            | "runpy"
            | "sched"
            | "secrets"
            | "select"
            | "selectors"
            | "shelve"
            | "shlex"
            | "shutil"
            | "signal"
            | "site"
            | "smtpd"
            | "smtplib"
            | "sndhdr"
            | "socket"
            | "socketserver"
            | "spwd"
            | "sqlite3"
            | "sre_compile"
            | "sre_constants"
            | "sre_parse"
            | "ssl"
            | "stat"
            | "statistics"
            | "string"
            | "stringprep"
            | "struct"
            | "subprocess"
            | "sunau"
            | "symbol"
            | "symtable"
            | "sys"
            | "sysconfig"
            | "syslog"
            | "tabnanny"
            | "tarfile"
            | "telnetlib"
            | "tempfile"
            | "termios"
            | "test"
            | "textwrap"
            | "threading"
            | "time"
            | "timeit"
            | "tkinter"
            | "token"
            | "tokenize"
            | "tomllib"
            | "trace"
            | "traceback"
            | "tracemalloc"
            | "tty"
            | "turtle"
            | "turtledemo"
            | "types"
            | "typing"
            | "unicodedata"
            | "unittest"
            | "urllib"
            | "uu"
            | "uuid"
            | "venv"
            | "warnings"
            | "wave"
            | "weakref"
            | "webbrowser"
            | "winreg"
            | "winsound"
            | "wsgiref"
            | "xdrlib"
            | "xml"
            | "xmlrpc"
            | "zipapp"
            | "zipfile"
            | "zipimport"
            | "zlib"
            | "_thread"
    )
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&PYTHON];
