use super::scope_analysis::ScopeAnalysis;
use super::syntax::{
    binding_scope, binding_scope_kind, qualified, text, walk_in_order_with_ancestors,
    BindingCollector, ScopeBindings,
};
use crate::languages::{
    context::{FileContext, LanguageContext, RuleIRHandle},
    profile::LanguageProfile,
    routes,
    types::{CompiledSyntaxGrammar, CompiledSyntaxKind, SyntaxContext},
};
use crate::models::{
    details::NodeDetailKey,
    *,
};
use crate::typed_facts::FlowStore;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node as Syntax;

#[cfg(feature = "lang-typescript")]
fn commonjs_default_callable(node: Syntax<'_>, source: &str) -> bool {
    crate::languages::typescript::commonjs_default_callable(node, source)
}

#[cfg(not(feature = "lang-typescript"))]
fn commonjs_default_callable(_node: Syntax<'_>, _source: &str) -> bool {
    false
}

fn declaration_signature(node: Syntax<'_>, source: &str) -> String {
    let mut c = node.walk();
    let body_node = node.child_by_field_name("body").or_else(|| {
        node.named_children(&mut c)
            .find(|ch| matches!(ch.kind(), "message_body" | "enum_body"))
    });
    let end = body_node.map_or(node.end_byte(), |b| b.start_byte());
    source[node.start_byte()..end]
        .trim()
        .trim_end_matches(';')
        .trim()
        .chars()
        .take(512)
        .collect()
}

fn declaration_signature_is_informative(node: Syntax<'_>, source: &str, language: &str) -> bool {
    if language == "rust"
        && node.named_children(&mut node.walk()).any(|child| {
            child.kind() == "visibility_modifier" && text(child, source).starts_with("pub(")
        })
    {
        return true;
    }
    let parameters = node
        .child_by_field_name("parameters")
        .or_else(|| node.child_by_field_name("formal_parameters"));
    let has_parameters = parameters.is_none_or(|parameters| parameters.named_child_count() > 0);
    if has_parameters
        || ["return_type", "type", "type_parameters", "type_arguments"]
            .into_iter()
            .any(|field| node.child_by_field_name(field).is_some())
    {
        return true;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    if !matches!(
        body.kind(),
        "block" | "statement_block" | "function_body" | "compound_statement"
    ) {
        return true;
    }
    let mut cursor = body.walk();
    let informative = body.named_children(&mut cursor).any(|child| {
        !child.kind().contains("comment")
            && !matches!(child.kind(), "pass_statement" | "empty_statement")
    });
    informative
}

fn declaration_signature_if_informative(
    node: Syntax<'_>,
    source: &str,
    language: &str,
) -> Option<String> {
    declaration_signature_is_informative(node, source, language)
        .then(|| declaration_signature(node, source))
}

pub(super) struct SinglePassExtractor<'a> {
    pub(super) source: &'a str,
    pub(super) path: &'a str,
    pub(super) profile: &'a dyn LanguageProfile,
    pub(super) language_context: &'a LanguageContext,
    pub(super) rule_ir: RuleIRHandle,
    pub(super) route_context: routes::RouteContext,
    pub(super) file: FileContext,
    pub(super) module: &'a str,
    pub(super) offset: usize,
    pub(super) symbols: HashMap<usize, String>,
    pub(super) ids: HashSet<String>,
    pub(super) flows: Option<&'a mut FlowStore>,
}
struct VisitContext<'a> {
    root_id: usize,
    source: &'a str,
    path: &'a str,
    profile: &'a dyn LanguageProfile,
    language: &'static str,
    route_extraction_enabled: bool,
    language_context: &'a LanguageContext,
    rule_ir: &'a RuleIRHandle,
    route_context: &'a routes::RouteContext,
    deferred_python: bool,
    syntax: Option<std::sync::Arc<CompiledSyntaxGrammar>>,
    file: &'a FileContext,
    module: &'a str,
    offset: usize,
}
struct VisitState<'a, 'flow> {
    symbols: &'a mut HashMap<usize, String>,
    ids: &'a mut HashSet<String>,
    flows: &'a mut Option<&'flow mut FlowStore>,
    root_summary: Option<ScopeBindings>,
    prepared_root_bindings: Option<ScopeBindings>,
    prepared_scope_bindings: HashMap<usize, ScopeBindings>,
    projections: HashMap<String, crate::semantic::ValueFlowFacts>,
}
#[derive(Clone, Copy)]
struct VisitedNode<'a, 'anc> {
    node: Syntax<'a>,
    kind: &'static str,
    ancestors: &'anc [Syntax<'a>],
}
struct ScopeTraversal<'scope, 'a> {
    frames: Vec<ScopeFrame<'a>>,
    scopes: &'scope mut Vec<String>,
    #[cfg(feature = "lang-python")]
    deferred: Vec<PythonAction<'a>>,
}
impl SinglePassExtractor<'_> {
    pub(super) fn visit(
        &mut self,
        node: Syntax<'_>,
        scopes: &mut Vec<String>,
        owner: &str,
        facts: &mut Facts,
    ) -> ScopeBindings {
        let deferred_python = self.profile.id() == "python" && self.route_context.uses_main_walk();
        let mut summary_builder = deferred_python.then(|| {
            routes::RouteContextBuilder::for_main_walk(node, self.source, self.language_context)
        });
        #[cfg(feature = "lang-python")]
        let mut class_bindings = {
            let has_class = self.profile.id() == "python" && self.source.contains("class");
            let registration = has_class && crate::languages::python::has_class_registration_import();
            let orm = has_class && self.language_context.semantic_rules.iter().any(|rule| rule.active && matches!(&rule.rule.operation, crate::languages::manifests::SemanticOperation::ClassDefaultTable { language, .. } if language == "python"));
            if has_class
                && node.kind() == "module"
                && !node.has_error()
                && !deferred_python
                && (registration || orm)
            {
                self.route_context.set_defer_python_orm();
                Some(routes::PythonClassBindings::new(
                    node,
                    self.source,
                    registration,
                ))
            } else {
                None
            }
        };
        let prepared_root_bindings = self
            .file
            .root_bindings
            .take()
            .filter(|(root, _)| *root == node.id())
            .map(|(_, bindings)| bindings);
        let prepared_scope_bindings = if self.profile.id() == "python" {
            self.route_context.take_python_bindings()
        } else {
            HashMap::new()
        };
        let context = VisitContext {
            root_id: node.id(),
            source: self.source,
            path: self.path,
            profile: self.profile,
            language: self.profile.id(),
            route_extraction_enabled: self.profile.route_extraction_enabled(),
            language_context: self.language_context,
            rule_ir: &self.rule_ir,
            route_context: &self.route_context,
            deferred_python,
            syntax: self.profile.syntax_grammar_spec().map(|spec| {
                spec.compile_cached(&node.language())
                    .expect("syntax spec validated before parsing")
            }),
            file: &self.file,
            module: self.module,
            offset: self.offset,
        };
        let mut state = VisitState {
            symbols: &mut self.symbols,
            ids: &mut self.ids,
            flows: &mut self.flows,
            root_summary: None,
            prepared_root_bindings,
            prepared_scope_bindings,
            projections: HashMap::new(),
        };
        let mut scope_traversal = ScopeTraversal {
            frames: Vec::new(),
            scopes,
            #[cfg(feature = "lang-python")]
            deferred: Vec::new(),
        };
        #[cfg(feature = "lang-python")]
        let mut main_guards = (context.language == "python")
            .then(crate::languages::python::lexical::MainGuardTracker::default);
        walk_in_order_with_ancestors(node, false, &mut |node, ancestors, entering| {
            let node_kind = node.kind();
            #[cfg(feature = "lang-python")]
            if entering {
                if let Some(tracker) = &mut main_guards {
                    tracker.observe(node, context.source);
                }
            }
            if let Some(builder) = &mut summary_builder {
                let prepared_bindings = builder.observe_with_kind(node, node_kind, entering);
                if !entering && prepared_bindings {
                    if let Some(bindings) = builder.take_bindings(node) {
                        state.prepared_scope_bindings.insert(node.id(), bindings);
                    }
                }
            }
            #[cfg(feature = "lang-python")]
            if let Some(builder) = &mut class_bindings {
                builder.observe(node, node_kind, ancestors, entering);
            }
            Self::visit_event(
                &context,
                &mut state,
                VisitedNode {
                    node,
                    kind: node_kind,
                    ancestors,
                },
                &mut scope_traversal,
                owner,
                facts,
                entering,
            );
        });
        #[cfg(feature = "lang-python")]
        if let Some(builder) = summary_builder {
            let lexical = builder.finish();
            finish_python_actions(
                &context,
                &mut state,
                facts,
                scope_traversal.deferred,
                &lexical,
            );
            crate::languages::python::finish_class_method_registrations(facts, &lexical);
        } else if let Some(builder) = class_bindings {
            let lexical = builder.finish();
            finish_python_actions(
                &context,
                &mut state,
                facts,
                scope_traversal.deferred,
                &lexical,
            );
            crate::languages::python::finish_class_method_registrations(facts, &lexical);
        } else if context.language == "python" {
            crate::languages::python::finish_class_method_registrations(
                facts,
                context.route_context,
            );
        }
        #[cfg(feature = "lang-python")]
        if let (Some(tracker), Some(bindings)) = (main_guards, state.root_summary.as_ref()) {
            tracker.finish(context.module, bindings, facts);
        }
        #[cfg(feature = "lang-typescript")]
        if matches!(context.language, "typescript" | "javascript") {
            crate::languages::typescript::finalize_configured_projections(
                facts,
                state.flows.as_deref_mut(),
                context.file,
            );
        }
        state.root_summary.unwrap_or(ScopeBindings {
            all: Vec::new(),
            rebindings: Vec::new(),
        })
    }

    fn visit_exit_event<'a>(
        context: &VisitContext<'a>,
        state: &mut VisitState<'_, '_>,
        event: VisitedNode<'a, '_>,
        traversal: &mut ScopeTraversal<'_, 'a>,
        owner: &str,
        facts: &mut Facts,
    ) {
        let node = event.node;
        if context.language == "python"
            && traversal
                .frames
                .first()
                .is_some_and(|root| root.suspended_at == Some(node.id()))
            && module_export_exit_kind(event.kind)
        {
            if let Some(root) = traversal.frames.first_mut() {
                root.observe_module_event(node, false);
            }
        }
        if route_candidate(context.route_extraction_enabled, context.language, event.kind) {
            Self::extract_exit_routes(context, state, event, traversal, owner, facts);
        }
        Self::exit_traversal_frames(context.language, node, traversal);
        if let Some(frame) = traversal
            .frames
            .pop_if(|frame| frame.root.id() == node.id())
        {
            let named_scope = frame.name.is_some();
            #[cfg(feature = "lang-python")]
            if context.deferred_python {
                traversal.deferred.push(PythonAction::Publication(
                    frame.prepare_publication(context, state),
                ));
            } else {
                frame.publish(context, state, facts);
            }
            #[cfg(not(feature = "lang-python"))]
            frame.publish(context, state, facts);
            if named_scope {
                traversal.scopes.pop();
            }
        } else if let Some(last) = traversal.frames.last_mut() {
            last.exit_metrics(node.id());
        }
    }

    fn extract_exit_routes<'a>(
        context: &VisitContext<'a>,
        state: &mut VisitState<'_, '_>,
        event: VisitedNode<'a, '_>,
        traversal: &mut ScopeTraversal<'_, 'a>,
        owner: &str,
        facts: &mut Facts,
    ) {
        let node_owner = traversal
            .frames
            .last()
            .map_or(owner, |frame| frame.owner.as_ref());
        let ctx = SyntaxContext::with_traversal(
            event.node,
            context.source,
            node_owner,
            context.offset,
            &context.file.state,
            event.kind,
            event.ancestors,
        );
        #[cfg(feature = "lang-python")]
        if context.deferred_python {
            if let Some(event) = crate::languages::python::prepare_deferred_event(
                &ctx,
                facts,
                state.symbols,
                context.language_context,
                context.rule_ir,
            ) {
                traversal.deferred.push(PythonAction::Candidate(event));
            }
            return;
        }
        #[cfg(feature = "lang-python")]
        if context.language == "python"
            && context.route_context.defer_python_orm()
            && event.kind == "class_definition"
            && event.node.child_by_field_name("superclasses").is_some()
        {
            traversal.deferred.push(PythonAction::Orm {
                node: event.node,
                owner: std::sync::Arc::from(node_owner),
                frontiers: (
                    facts.nodes.len(),
                    facts.edges.len(),
                    facts.references.len(),
                ),
            });
        }
        context.profile.extract_routes_with_typed_projections(
            &ctx,
            facts,
            state.symbols,
            context.language_context,
            context.rule_ir,
            context.route_context,
            &mut state.projections,
        );
    }

    fn exit_traversal_frames<'a>(
        language: &str,
        node: Syntax<'a>,
        traversal: &mut ScopeTraversal<'_, 'a>,
    ) {
        if language == "python" {
            let mut first_effective_frame = traversal.frames.len();
            for (index, frame) in traversal.frames.iter().enumerate().rev() {
                if frame
                    .suspended_at
                    .is_some_and(|suspended_at| suspended_at != node.id())
                {
                    break;
                }
                first_effective_frame = index;
            }
            for frame in traversal.frames.iter_mut().skip(first_effective_frame) {
                frame.exit(node);
            }
        } else {
            for frame in traversal.frames.iter_mut() {
                frame.exit(node);
            }
        }
    }

    fn enter_module_root_frame<'a>(
        context: &VisitContext<'a>,
        state: &mut VisitState<'_, '_>,
        event: VisitedNode<'a, '_>,
        traversal: &mut ScopeTraversal<'_, 'a>,
        owner: &str,
        facts: &mut Facts,
        is_boundary: bool,
    ) {
        let (node, node_kind) = (event.node, event.kind);
        let prepared_bindings = state
            .prepared_scope_bindings
            .remove(&node.id())
            .filter(|_| state.prepared_root_bindings.is_none());
        let mut frame = ScopeFrame::new(
            node,
            std::sync::Arc::from(owner),
            None,
            state.prepared_root_bindings.is_none().then_some(node_kind),
            false,
            context,
            prepared_bindings,
        );
        frame.enter(node, node_kind, is_boundary);
        traversal.frames.push(frame);
        let Some(module_index) = facts.nodes.iter().position(|n| n.id == owner) else {
            return;
        };
        traversal
            .frames
            .last_mut()
            .expect("module frame entered")
            .node_index = Some(module_index);
        let module = &mut facts.nodes[module_index];
        #[cfg(feature = "lang-python")]
        if context.language == "python" {
            let factories = crate::languages::python::collect_logging_factories(
                node,
                context.source,
                context.offset,
            );
            if factories
                .as_array()
                .is_some_and(|entries| !entries.is_empty())
            {
                module.set_detail(NodeDetailKey::PYTHON_LOGGING_FACTORIES, factories);
            }
        }
        #[cfg(feature = "lang-typescript")]
        if matches!(context.language, "javascript" | "typescript" | "vue") {
            let empty_shadows = HashSet::new();
            let shadowed = context
                .file
                .state
                .typescript()
                .map_or(&empty_shadows, |state| &*state.shadowed_require_scopes);
            module.set_detail(
                NodeDetailKey::COMMONJS_BINDINGS,
                json!(crate::languages::typescript::commonjs_bindings(
                    node,
                    context.source,
                    context.offset,
                    shadowed,
                )),
            );
        }
        if context.language == "python" {
            module.set_detail(NodeDetailKey::LAZY_EXPORTS, serde_json::Value::Null);
        }
        module.set_detail(NodeDetailKey::EXPORTS, serde_json::Value::Null);
        let details = module
            .details
            .as_object_mut()
            .expect("module details object");
        let frame = traversal.frames.last_mut().expect("module frame entered");
        frame.reserved_flow_slot = !details.contains_key(NodeDetailKey::VALUE_FLOW);
        details
            .entry(NodeDetailKey::VALUE_FLOW)
            .or_insert(serde_json::Value::Null);
        details
            .entry(NodeDetailKey::BINDINGS)
            .or_insert_with(|| json!([]));
        details
            .entry(NodeDetailKey::REBINDINGS)
            .or_insert_with(|| json!([]));
    }

    fn record_syntax_diagnostic<'a>(
        context: &VisitContext<'a>,
        node: Syntax<'a>,
        facts: &mut Facts,
    ) {
        let is_ts_grammar_gap = context.language == "typescript" && {
            let line_text = context
                .source
                .lines()
                .nth(node.start_position().row)
                .unwrap_or("");
            line_text.contains("import(")
                || line_text.contains("readonly [")
                || line_text.contains("readonly (")
        };
        if !is_ts_grammar_gap {
            facts.errors.push(Diagnostic {
                path: context.path.into(),
                line: node.start_position().row + context.offset + 1,
                message: format!(
                    "syntax {} at column {}",
                    node.kind(),
                    node.start_position().column + 1
                ),
            });
        }
    }

    fn enter_declaration_node<'a>(
        context: &VisitContext<'a>,
        state: &mut VisitState<'_, '_>,
        event: VisitedNode<'a, '_>,
        traversal: &mut ScopeTraversal<'_, 'a>,
        facts: &mut Facts,
        kind: &str,
        (owner, is_boundary): (&str, bool),
    ) {
        let (node, node_kind, ancestors) = (event.node, event.kind, event.ancestors);
        let (source, path, language, module, offset) = (
            context.source,
            context.path,
            context.language,
            context.module,
            context.offset,
        );
        let kind = crate::models::compact_graph::CompactTag::known(kind).unwrap_or(kind);
        let language =
            crate::models::compact_graph::CompactTag::known(language).unwrap_or(language);
        let line = node.start_position().row + offset + 1;
        let name = context
            .profile
            .symbol_name(node, source)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("anonymous@{}:{}", line, node.start_position().column + 1)
            });
        let prefix = context.profile.node_prefix(kind);
        let mut id = format!("{prefix}:{path}:{line}:{name}");
        if !state.ids.insert(id.clone()) {
            use std::fmt::Write;
            let _ = write!(id, ":{}", node.start_position().column);
            state.ids.insert(id.clone());
        }
        state.symbols.insert(node.id(), id.clone());
        let shared_id: std::sync::Arc<str> = std::sync::Arc::from(id.as_str());
        let shared_name: std::sync::Arc<str> = std::sync::Arc::from(name.as_str());
        let prepared_bindings = state.prepared_scope_bindings.remove(&node.id());
        let is_callable = matches!(kind, "function" | "method" | "constructor");
        let mut frame = ScopeFrame::new(
            node,
            std::sync::Arc::clone(&shared_id),
            Some(shared_name),
            Some(node_kind),
            is_callable,
            context,
            prepared_bindings,
        );
        let mut metadata = if frame.analysis.is_some() {
            context.profile.metadata_with_ancestors(
                node,
                node_kind,
                ancestors,
                source,
                &name,
                context.file,
            )
        } else {
            let (metadata, flow) =
                context
                    .profile
                    .declaration(node, source, &name, context.file);
            frame.legacy_flow = Some(flow);
            metadata
        };
        let test_role = context.profile.test_role(node, source, path, context.file);
        let prototype_owner = (language == "javascript"
            && matches!(node_kind, "function_expression" | "arrow_function")
            && metadata.is_method == Some(true))
        .then(|| metadata.receiver_type.clone())
        .flatten();
        let out_of_line_owner = (matches!(
            (language, node_kind),
            ("cpp", "function_definition") | ("go", "method_declaration")
        ) && metadata.is_method == Some(true))
        .then(|| {
            metadata
                .receiver_type
                .as_deref()
                .filter(|owner| traversal.scopes.last().map(String::as_str) != Some(*owner))
        })
        .flatten();
        let qualname = if let Some(receiver) =
            prototype_owner.as_deref().or(out_of_line_owner)
        {
            qualified(module, traversal.scopes, &format!("{receiver}.{name}"))
        } else {
            qualified(module, traversal.scopes, &name)
        };
        let mut details = serde_json::Map::new();
        if let Some(receiver_name) = metadata.receiver_name.filter(|value| !value.is_empty()) {
            details.insert("receiver_name".into(), json!(receiver_name));
        }
        if metadata.default_export {
            details.insert("default_export".into(), json!(true));
        }
        let doc = syntax_doc(context, node);
        if !doc.is_empty() {
            details.insert("doc".into(), json!(doc));
        }
        if !metadata.decorators.is_empty() {
            details.insert("decorators".into(), json!(metadata.decorators));
        }
        if metadata
            .bases
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            details.insert("bases".into(), json!(metadata.bases));
        }
        if metadata
            .receiver
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            details.insert("receiver".into(), json!(metadata.receiver));
        }
        if let Some(signature) = declaration_signature_if_informative(node, source, language) {
            details.insert("signature".into(), json!(signature));
        }
        if metadata.is_async {
            details.insert("async".into(), json!(true));
        }
        if let Some(test_role) = test_role {
            details.insert("test_role".into(), json!(test_role));
        }
        if let Some(entrypoint) = if test_role == Some("test_case") {
            Some("test")
        } else {
            context
                .profile
                .entrypoint_role(node, source, &name, context.file)
        } {
            details.insert("entrypoint_base".into(), json!(entrypoint));
        }
        if let Some(is_public) = metadata.is_public {
            details.insert("is_public".into(), json!(is_public));
        }
        if let Some(receiver_type) = metadata.receiver_type.as_ref() {
            details.insert("receiver_type".into(), json!(receiver_type));
        }
        if prototype_owner.is_some() {
            details.insert("prototype".into(), json!(true));
        }
        if language == "javascript" && commonjs_default_callable(node, source) {
            details.insert("commonjs_default_callable".into(), json!(true));
        }
        if metadata.is_method == Some(true) && kind != "method" {
            details.insert("is_method".into(), json!(true));
        }
        if metadata.is_static == Some(true) {
            details.insert("is_static".into(), json!(true));
        }
        let native_semantic = if !metadata.param_types.is_empty() {
            if state.flows.is_some() {
                details.insert("param_types".into(), serde_json::Value::Null);
                Some(Box::new(
                    crate::typed_facts::NodeSemanticEntry::with_parameters(std::mem::take(
                        &mut metadata.param_types,
                    )),
                ))
            } else {
                details.insert("param_types".into(), json!(metadata.param_types));
                None
            }
        } else {
            None
        };
        details.insert("column".into(), json!(node.start_position().column));
        if metadata.is_overload {
            details.insert("is_overload".into(), json!(true));
        }
        if metadata.is_stub {
            details.insert("is_stub".into(), json!(true));
        }
        if metadata.is_conditional {
            details.insert("is_conditional".into(), json!(true));
        }
        let details = if details.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(details)
        };
        let node_index = facts.nodes.len();
        facts.nodes.push(Node {
            native_semantic,
            id: id.clone(),
            kind: crate::models::tags::NodeKind::parse(kind).expect("recognized declaration kind"),
            name: name.clone(),
            qualname,
            path: path.into(),
            line,
            end_line: node.end_position().row + offset + 1,
            is_test: is_test(path)
                || test_role.is_some()
                || context.profile.test_attribute(node, source),
            language: crate::models::tags::LanguageId::parse(language).expect("recognized language profile"),
            generated: false,
            details,
        });
        let parent_owner = traversal
            .frames
            .last()
            .map_or(owner, |frame| frame.owner.as_ref());
        frame.node_index = Some(node_index);
        facts.edges.push(Edge {
            src: parent_owner.into(),
            dst: id.clone(),
            kind: "contains".into(),
            path: path.into(),
            line,
            evidence: node_kind.into(),
            confidence: "exact".into(),
        });
        context.profile.extract_relations(
            &SyntaxContext::with_traversal(
                node,
                source,
                &id,
                offset,
                &context.file.state,
                node_kind,
                ancestors,
            ),
            facts,
        );
        frame.reserved_flow_slot = true;
        frame.metadata_return = metadata.return_type;
        frame.metadata_ownership = metadata.object_ownership;
        frame.enter(node, node_kind, is_boundary);
        traversal.frames.push(frame);
        traversal.scopes.push(name);
    }

    fn emit_node_references<'a>(
        context: &VisitContext<'a>,
        event: VisitedNode<'a, '_>,
        syntax_kind: Option<CompiledSyntaxKind>,
        node_owner: &str,
        facts: &mut Facts,
    ) {
        let (node, node_kind) = (event.node, event.kind);
        let ctx = SyntaxContext::with_traversal(
            node,
            context.source,
            node_owner,
            context.offset,
            &context.file.state,
            node_kind,
            event.ancestors,
        );
        if context
            .syntax
            .as_ref()
            .is_none_or(|_| syntax_kind.is_some_and(|kind| kind.is_import()))
        {
            context.profile.extract_imports(&ctx, facts);
        }
        if context
            .syntax
            .as_ref()
            .is_none_or(|_| syntax_kind.is_some_and(|kind| kind.is_reference()))
        {
            context.profile.extract_calls(&ctx, facts);
        }
        if context
            .syntax
            .as_ref()
            .is_some_and(|_| syntax_kind.is_some_and(|kind| kind.is_member_access()))
        {
            facts.references.push(crate::models::Reference {
                source: node_owner.to_owned(),
                expression: text(node, context.source).into(),
                kind: "reads".into(),
                line: ctx.line(),
                column: node.start_position().column,
                alias: None,
                module: None,
                dynamic: false,
                receiver_hint: None,
            });
        }
        if matches!(node_kind, "call" | "call_expression") {
            routes::dependency_injection(&ctx, context.rule_ir, facts);
        }
        if context
            .syntax
            .as_ref()
            .is_none_or(|_| syntax_kind.is_some_and(CompiledSyntaxKind::is_mutation))
        {
            context.profile.extract_mutations(&ctx, facts);
        }
    }

    fn visit_event<'a>(
        context: &VisitContext<'a>,
        state: &mut VisitState<'_, '_>,
        event: VisitedNode<'a, '_>,
        traversal: &mut ScopeTraversal<'_, 'a>,
        owner: &str,
        facts: &mut Facts,
        entering: bool,
    ) {
        if !entering {
            Self::visit_exit_event(context, state, event, traversal, owner, facts);
            return;
        }
        let (node, node_kind) = (event.node, event.kind);
        let is_boundary = scope_boundary_kind(context.language, node_kind);
        let syntax_kind = context
            .syntax
            .as_ref()
            .and_then(|syntax| syntax.classify(node.kind_id()));
        for frame in traversal.frames.iter_mut() {
            if frame.suspended_at.is_some() {
                frame.enter_suspended(node);
            } else {
                frame.enter(node, node_kind, is_boundary);
            }
        }
        if context.language == "python"
            && traversal
                .frames
                .first()
                .is_some_and(|root| root.suspended_at.is_none())
            && module_export_enter_kind(node_kind)
        {
            if let Some(root) = traversal.frames.first_mut() {
                root.observe_module_event(node, true);
            }
        }
        let module_root = node.id() == context.root_id;
        if module_root {
            Self::enter_module_root_frame(context, state, event, traversal, owner, facts, is_boundary);
        }
        if node.is_error() || node.is_missing() {
            Self::record_syntax_diagnostic(context, node, facts);
        }
        let kind = match context.syntax.as_ref() {
            Some(_) => syntax_kind
                .and_then(CompiledSyntaxKind::declaration)
                .and_then(|decl| {
                    if context.profile.symbol_kind(node_kind).is_some() {
                        context.profile.symbol_with_ancestors(
                            node,
                            node_kind,
                            event.ancestors,
                            context.source,
                            context.file,
                        )
                    } else {
                        Some(decl)
                    }
                }),
            None => context.profile.symbol_with_ancestors(
                node,
                node_kind,
                event.ancestors,
                context.source,
                context.file,
            ),
        };
        if let Some(kind) = kind {
            Self::enter_declaration_node(
                context,
                state,
                event,
                traversal,
                facts,
                kind,
                (owner, is_boundary),
            );
        } else if !module_root {
            if let Some(last) = traversal.frames.last_mut() {
                last.enter_metrics(node, node_kind, context.source, context.offset);
            }
        }
        let node_owner = traversal
            .frames
            .last()
            .map_or(owner, |frame| frame.owner.as_ref());
        Self::emit_node_references(context, event, syntax_kind, node_owner, facts);
    }
}

fn route_candidate(enabled: bool, language: &str, node_kind: &str) -> bool {
    if !enabled {
        return false;
    }
    match language {
        "python" => matches!(
            node_kind,
            "function_definition"
                | "class_definition"
                | "call"
                | "assignment"
                | "delete_statement"
                | "augmented_assignment"
        ),
        "javascript" | "typescript" | "vue" => matches!(
            node_kind,
            "method_definition"
                | "method_declaration"
                | "call_expression"
                | "object"
                | "member_expression"
        ),
        _ => true,
    }
}

#[cfg(feature = "lang-python")]
enum PythonAction<'tree> {
    Candidate(crate::languages::python::DeferredEvent<'tree>),
    Orm {
        node: Syntax<'tree>,
        owner: std::sync::Arc<str>,
        frontiers: (usize, usize, usize),
    },
    Publication(ScopePublication<'tree>),
}

#[cfg(feature = "lang-python")]
fn finish_python_actions(
    context: &VisitContext<'_>,
    state: &mut VisitState<'_, '_>,
    facts: &mut Facts,
    actions: Vec<PythonAction<'_>>,
    lexical: &routes::RouteContext,
) {
    let base = (facts.nodes.len(), facts.edges.len(), facts.references.len());
    let mut node_batches = Vec::new();
    let mut edge_batches = Vec::new();
    let mut reference_batches = Vec::new();
    for action in actions {
        match action {
            PythonAction::Publication(publication) => publication.publish(context, state, facts),
            PythonAction::Orm {
                node,
                owner,
                frontiers,
            } => {
                let before = (facts.nodes.len(), facts.edges.len(), facts.references.len());
                let ctx = SyntaxContext::new(
                    node,
                    context.source,
                    &owner,
                    context.offset,
                    &context.file.state,
                );
                crate::languages::python::finish_default_table(
                    &ctx,
                    facts,
                    context.language_context,
                    lexical,
                );
                node_batches.push((frontiers.0, facts.nodes.len() - before.0));
                edge_batches.push((frontiers.1, facts.edges.len() - before.1));
                reference_batches.push((frontiers.2, facts.references.len() - before.2));
            }
            PythonAction::Candidate(event) => {
                let before = (facts.nodes.len(), facts.edges.len(), facts.references.len());
                let ctx = SyntaxContext::new(
                    event.node,
                    context.source,
                    &event.owner,
                    context.offset,
                    &context.file.state,
                );
                crate::languages::python::finish_deferred_event(
                    &event,
                    &ctx,
                    facts,
                    state.symbols,
                    context.language_context,
                    context.rule_ir,
                    lexical,
                    &mut state.projections,
                );
                node_batches.push((event.frontiers.0, facts.nodes.len() - before.0));
                edge_batches.push((event.frontiers.1, facts.edges.len() - before.1));
                reference_batches.push((event.frontiers.2, facts.references.len() - before.2));
            }
        }
    }
    facts.remap_deferred_nodes(base.0, &node_batches);
    restore_append_order(&mut facts.nodes, base.0, node_batches);
    restore_append_order(&mut facts.edges, base.1, edge_batches);
    restore_append_order(&mut facts.references, base.2, reference_batches);
}

#[cfg(feature = "lang-python")]
fn restore_append_order<T>(values: &mut Vec<T>, base_len: usize, batches: Vec<(usize, usize)>) {
    if values.len() == base_len {
        return;
    }
    let append_only =
        batches
            .iter()
            .try_fold((0usize, 0usize), |(total, previous), &(frontier, count)| {
                if frontier < previous
                    || frontier > base_len
                    || (count != 0 && frontier != base_len)
                {
                    return None;
                }
                total.checked_add(count).map(|total| (total, frontier))
            });
    if append_only.is_some_and(|(total, _)| total == values.len() - base_len) {
        return;
    }
    let mut original = std::mem::take(values);
    let appended = original.split_off(base_len);
    let mut original = original.into_iter();
    let mut appended = appended.into_iter();
    let mut cursor = 0;
    for (frontier, count) in batches {
        values.extend(original.by_ref().take(frontier - cursor));
        cursor = frontier;
        values.extend(appended.by_ref().take(count));
    }
    values.extend(original);
    debug_assert!(appended.next().is_none());
}

struct ScopeFrame<'a> {
    root: Syntax<'a>,
    owner: std::sync::Arc<str>,
    node_index: Option<usize>,
    name: Option<std::sync::Arc<str>>,
    metadata_return: Option<crate::semantic::TypeExpr>,
    metadata_ownership: Option<crate::semantic::ObjectOwnershipFact>,
    bindings: Option<BindingCollector<'a, 'a>>,
    prepared_bindings: Option<ScopeBindings>,
    analysis: Option<ScopeAnalysis<'a, 'a>>,
    legacy_flow: Option<crate::semantic::ValueFlowFacts>,
    metrics: Option<super::fingerprints::CallableMetricsTracker>,
    reserved_flow_slot: bool,
    suspended_at: Option<usize>,
}
impl<'a> ScopeFrame<'a> {
    fn new(
        root: Syntax<'a>,
        owner: std::sync::Arc<str>,
        name: Option<std::sync::Arc<str>>,
        collect_bindings_kind: Option<&'static str>,
        is_callable: bool,
        context: &VisitContext<'a>,
        prepared_bindings: Option<ScopeBindings>,
    ) -> Self {
        Self {
            root,
            owner,
            node_index: None,
            name,
            metadata_return: None,
            metadata_ownership: None,
            bindings: collect_bindings_kind
                .filter(|_| {
                    prepared_bindings.is_none()
                        && !(context.deferred_python
                            && (root.id() == context.root_id || binding_scope(root)))
                })
                .map(|kind| BindingCollector::new_with_kind(root, context.source, kind)),
            prepared_bindings,
            analysis: context
                .profile
                .scope_analysis(root, context.source, context.file)
                .or_else(|| {
                    context
                        .syntax
                        .as_ref()
                        .filter(|compiled| compiled.has_value_flow_selectors())
                        .map(|compiled| {
                            ScopeAnalysis::generic(
                                root,
                                context.source,
                                std::sync::Arc::clone(compiled),
                            )
                        })
                }),
            legacy_flow: None,
            metrics: is_callable.then(super::fingerprints::CallableMetricsTracker::default),
            reserved_flow_slot: false,
            suspended_at: None,
        }
    }
    fn enter_metrics(
        &mut self,
        node: Syntax<'a>,
        node_kind: &str,
        source: &str,
        offset: usize,
    ) {
        if let Some(metrics) = &mut self.metrics {
            super::fingerprints::CallableMetricsTracker::enter(
                metrics,
                node,
                node_kind,
                self.root.id(),
                source,
                offset,
            );
        }
    }
    fn exit_metrics(&mut self, node_id: usize) {
        if let Some(metrics) = &mut self.metrics {
            super::fingerprints::CallableMetricsTracker::exit(metrics, node_id);
        }
    }
    fn enter(&mut self, node: Syntax<'a>, node_kind: &str, is_boundary: bool) {
        if let Some(bindings) = &mut self.bindings {
            bindings.enter_with_kind(node, node_kind);
        }
        if let Some(analysis) = &mut self.analysis {
            analysis.enter_with_kind(node, node_kind);
        }
        if is_boundary && node.id() != self.root.id() {
            self.suspended_at = Some(node.id());
        }
    }
    fn observe_module_event(&mut self, node: Syntax<'a>, entering: bool) {
        if self.root.kind() == "module" {
            if let Some(analysis) = &mut self.analysis {
                analysis.observe_module_event(node, entering);
            }
        }
    }
    fn enter_suspended(&mut self, node: Syntax<'a>) {
        if let Some(analysis) = &mut self.analysis {
            analysis.enter_suspended(node);
        }
    }
    fn exit(&mut self, node: Syntax<'a>) {
        if let Some(suspended_at) = self.suspended_at {
            if suspended_at != node.id() {
                if let Some(analysis) = &mut self.analysis {
                    analysis.exit_suspended(node);
                }
                return;
            }
            self.suspended_at = None;
        }
        if let Some(bindings) = &mut self.bindings {
            bindings.exit();
        }
        if let Some(analysis) = &mut self.analysis {
            analysis.exit(node);
        }
    }
    fn prepare_publication(
        self,
        context: &VisitContext<'_>,
        state: &mut VisitState<'_, '_>,
    ) -> ScopePublication<'a> {
        let collected_bindings = state
            .prepared_scope_bindings
            .remove(&self.root.id())
            .or(self.prepared_bindings);
        let prepared_bindings = if self.name.is_none() {
            state.prepared_root_bindings.take().or(collected_bindings)
        } else {
            collected_bindings
        };
        let mut bindings = if let Some(bindings) = prepared_bindings {
            bindings
        } else if context.syntax.as_ref().is_some_and(|syntax| {
            !syntax
                .classify(self.root.kind_id())
                .is_some_and(|kind| kind.is_scope())
        }) {
            ScopeBindings {
                all: Vec::new(),
                rebindings: Vec::new(),
            }
        } else if self.analysis.is_some() || context.syntax.is_some() {
            self.bindings
                .expect("unprepared scope collects bindings")
                .finish()
        } else {
            context.profile.bindings(self.root, context.source)
        };
        let mut module_outputs = None;
        let mut flow = if let Some(analysis) = self.analysis {
            if self.name.is_none() {
                module_outputs = analysis.module_exports(&bindings);
            }
            let (flow, aliases) = analysis.finish(self.root.kind() == "module");
            if self.root.kind() == "module" {
                if let Some(aliases) = aliases {
                    bindings.rebindings.retain(|name| !aliases.contains(name));
                }
            }
            flow
        } else if let Some(flow) = self.legacy_flow {
            flow
        } else {
            context
                .profile
                .value_flow_with_context(self.root, context.source, context.file)
        };
        if flow.return_type.is_none() {
            flow.return_type = self.metadata_return;
        }
        if let Some(ownership) = self.metadata_ownership {
            flow.object_ownership = Some(ownership);
        }
        flow.offset_lines(context.offset);
        #[cfg(feature = "lang-html")]
        if let Some(framework) = flow.framework.as_mut() {
            for template in framework.template_contexts_mut() {
                template.target_path = crate::languages::html::local_template_target(
                    context.path,
                    &template.target,
                    context.language_context,
                );
            }
        }
        let metrics = self
            .metrics
            .map(|m| (m.cognitive_complexity, m.max_nesting_depth, m.statements));
        ScopePublication {
            root: self.root,
            owner: self.owner,
            node_index: self.node_index,
            name: self.name,
            reserved_flow_slot: self.reserved_flow_slot,
            flow: (!(flow.is_empty() && flow.return_position.is_none())).then(|| Box::new(flow)),
            bindings,
            metrics,
            module_outputs,
        }
    }

    fn publish(
        self,
        context: &VisitContext<'_>,
        state: &mut VisitState<'_, '_>,
        facts: &mut Facts,
    ) {
        self.prepare_publication(context, state)
            .publish(context, state, facts);
    }
}

struct ScopePublication<'tree> {
    root: Syntax<'tree>,
    owner: std::sync::Arc<str>,
    node_index: Option<usize>,
    name: Option<std::sync::Arc<str>>,
    reserved_flow_slot: bool,
    flow: Option<Box<crate::semantic::ValueFlowFacts>>,
    bindings: ScopeBindings,
    metrics: Option<(usize, usize, Vec<StatementFingerprint>)>,
    module_outputs: Option<(Vec<crate::semantic::ExportBinding>, serde_json::Value)>,
}

fn retain_scope_framework_facts(facts: &mut Vec<crate::semantic::FrameworkFact>) {
    facts.retain(|fact| {
        matches!(
            fact,
            crate::semantic::FrameworkFact::ComponentConfiguration(_)
                | crate::semantic::FrameworkFact::SymbolAttachment(_)
                | crate::semantic::FrameworkFact::NamedBinding(_)
                | crate::semantic::FrameworkFact::EndpointRequest(_)
        )
    });
}

fn merge_legacy_framework_array<T: serde::de::DeserializeOwned>(
    flow: &mut crate::semantic::ValueFlowFacts,
    raw_flow: &serde_json::Value,
    key: &str,
    label: &str,
    is_kind: impl Fn(&crate::semantic::FrameworkFact) -> bool,
    push: impl Fn(&mut crate::semantic::ValueFlowFacts, T),
) {
    let Some(value) = raw_flow.get(key) else {
        return;
    };
    flow.framework
        .get_or_insert_with(Default::default)
        .facts
        .retain(|fact| !is_kind(fact));
    let entries: Vec<T> = serde_json::from_value(value.clone())
        .unwrap_or_else(|err| panic!("parser-owned {label}: {err}"));
    for entry in entries {
        push(flow, entry);
    }
}

fn merge_scope_projections(
    owner: &str,
    context: &VisitContext<'_>,
    state: &mut VisitState<'_, '_>,
    node: &Node,
    flow: &mut crate::semantic::ValueFlowFacts,
) {
    if let Some(projections) = state.projections.remove(owner) {
        if projections.fact_limit_exceeded {
            flow.fact_limit_exceeded = true;
        }
        if let Some(mut projections) = projections.framework {
            retain_scope_framework_facts(&mut projections.facts);
            if !projections.facts.is_empty() {
                flow.framework
                    .get_or_insert_with(Default::default)
                    .facts
                    .append(&mut projections.facts);
            }
        }
    }
    if context.profile.typed_scope_projections() {
        return;
    }
    let raw_flow = node.detail_entry(NodeDetailKey::VALUE_FLOW);
    merge_legacy_framework_array(
        flow,
        raw_flow,
        "component_configurations",
        "typed component configurations",
        |f| matches!(f, crate::semantic::FrameworkFact::ComponentConfiguration(_)),
        |flow, fact| flow.push_component_configuration(fact),
    );
    merge_legacy_framework_array(
        flow,
        raw_flow,
        "symbol_attachments",
        "symbol attachments",
        |f| matches!(f, crate::semantic::FrameworkFact::SymbolAttachment(_)),
        |flow, fact| flow.push_symbol_attachment(fact),
    );
    merge_legacy_framework_array(
        flow,
        raw_flow,
        "named_bindings",
        "named bindings",
        |f| matches!(f, crate::semantic::FrameworkFact::NamedBinding(_)),
        |flow, fact| flow.push_named_binding(fact),
    );
    merge_legacy_framework_array(
        flow,
        raw_flow,
        "endpoint_requests",
        "endpoint requests",
        |f| matches!(f, crate::semantic::FrameworkFact::EndpointRequest(_)),
        |flow, fact| flow.push_endpoint_request(fact),
    );
    if raw_flow["fact_limit_exceeded"] == true {
        flow.fact_limit_exceeded = true;
    }
}

fn set_optional_json_detail(node: &mut Node, key: &'static str, value: Option<serde_json::Value>) {
    if let Some(value) = value {
        node.set_detail(key, value);
    } else {
        node.remove_detail(key);
    }
}

fn take_previous_scope_flow(
    node: &Node,
    state: &mut VisitState<'_, '_>,
    preserve_invalid: &mut bool,
) -> Option<crate::semantic::ValueFlowFacts> {
    if let Some(flows) = state.flows.as_mut() {
        if matches!(
            flows.state(node),
            Some(crate::typed_facts::FlowState::Invalid(_))
        ) {
            *preserve_invalid = true;
            return None;
        }
        return flows.take(&node.id);
    }
    let raw = node.detail_entry(NodeDetailKey::VALUE_FLOW);
    if raw.is_null() {
        return None;
    }
    if raw["fact_limit_exceeded"] == true
        || crate::typed_facts::encoded_scope_fact_count(raw)
            > crate::semantic::DEFAULT_FACTS_PER_SCOPE
    {
        *preserve_invalid = true;
        return None;
    }
    match crate::semantic::ValueFlowFacts::deserialize(raw) {
        Ok(previous) => Some(previous),
        Err(_) => {
            *preserve_invalid = true;
            None
        }
    }
}

fn merge_previous_scope_flow(
    context: &VisitContext<'_>,
    state: &mut VisitState<'_, '_>,
    node: &Node,
    flow: &mut crate::semantic::ValueFlowFacts,
) -> bool {
    let mut preserve_invalid = false;
    if flow.is_empty() {
        return false;
    }
    if let Some(mut previous) = take_previous_scope_flow(node, state, &mut preserve_invalid) {
        if context.profile.typed_scope_projections() {
            if let Some(mut projections) = previous.framework.take() {
                retain_scope_framework_facts(&mut projections.facts);
                if !projections.facts.is_empty() {
                    let mut current = flow.framework.take().unwrap_or_default();
                    projections.facts.append(&mut current.facts);
                    current.facts = projections.facts;
                    flow.framework = Some(current);
                }
            }
            if previous.fact_limit_exceeded {
                flow.fact_limit_exceeded = true;
            }
        }
        flow.bindings.extend(previous.bindings);
        flow.immutable_initializers
            .extend(previous.immutable_initializers);
        flow.fields.extend(previous.fields);
    }
    flow.bindings.sort_by_key(|v| v.position);
    flow.immutable_initializers.sort_unstable();
    flow.fields.sort_by_key(|v| v.position);
    preserve_invalid
}

fn write_scope_bindings(
    node: &mut Node,
    state: &VisitState<'_, '_>,
    bindings: ScopeBindings,
) {
    if state.flows.is_some() {
        crate::typed_facts::NodeSemanticEntry::set_bindings(
            node,
            bindings.all,
            bindings.rebindings,
        );
        return;
    }
    set_optional_json_detail(
        node,
        NodeDetailKey::BINDINGS,
        (!bindings.all.is_empty()).then(|| json!(bindings.all)),
    );
    set_optional_json_detail(
        node,
        NodeDetailKey::REBINDINGS,
        (!bindings.rebindings.is_empty()).then(|| json!(bindings.rebindings)),
    );
}

fn write_scope_flow(
    node: &mut Node,
    context: &VisitContext<'_>,
    state: &mut VisitState<'_, '_>,
    boxed: Box<crate::semantic::ValueFlowFacts>,
) -> bool {
    let is_overflow = |flow_state: Option<&crate::typed_facts::FlowState>| {
        matches!(
            flow_state,
            Some(crate::typed_facts::FlowState::Invalid(v)) if v["fact_limit_exceeded"] == true
        )
    };
    let mut fact_limit_exceeded = boxed.fact_limit_exceeded;
    if let Some(flows) = state.flows.as_mut() {
        set_optional_json_detail(node, NodeDetailKey::VALUE_FLOW, None);
        flows.insert_boxed(node.id.clone(), boxed);
        fact_limit_exceeded |= is_overflow(flows.state(node));
    } else if context.profile.typed_scope_projections() {
        let mut validated = FlowStore::default();
        validated.insert_boxed(node.id.clone(), boxed);
        fact_limit_exceeded |= is_overflow(validated.state(node));
        node.set_detail(
            NodeDetailKey::VALUE_FLOW,
            json!(validated.state(node).expect("published scope flow").view()),
        );
    } else {
        node.set_detail(NodeDetailKey::VALUE_FLOW, json!(boxed.as_ref()));
    }
    fact_limit_exceeded
}

impl ScopePublication<'_> {
    fn publish(
        self,
        context: &VisitContext<'_>,
        state: &mut VisitState<'_, '_>,
        facts: &mut Facts,
    ) {
        let ScopePublication {
            root,
            owner,
            node_index,
            name,
            mut bindings,
            module_outputs,
            flow: mut prepared_flow,
            reserved_flow_slot,
            metrics,
        } = self;
        let Some(node) = node_index
            .and_then(|index| facts.nodes.get_mut(index))
            .filter(|node| node.id == *owner)
        else {
            return;
        };
        let mut empty_flow = crate::semantic::ValueFlowFacts::default();
        let flow = prepared_flow.as_deref_mut().unwrap_or(&mut empty_flow);
        merge_scope_projections(&owner, context, state, node, flow);

        let mut visibility_exports = None;
        let mut preserve_invalid = false;
        if name.is_none() {
            let exports = match module_outputs {
                Some((exports, lazy_exports)) => {
                    let has_lazy = lazy_exports
                        .as_array()
                        .is_some_and(|entries| !entries.is_empty());
                    set_optional_json_detail(node, NodeDetailKey::LAZY_EXPORTS, has_lazy.then_some(lazy_exports));
                    Some(exports)
                }
                None => context
                    .profile
                    .module_exports_with_bindings(root, context.source, &bindings),
            };
            visibility_exports = exports.clone();
            set_optional_json_detail(
                node,
                NodeDetailKey::EXPORTS,
                exports.filter(|e| !e.is_empty()).map(|e| json!(e)),
            );
            state.root_summary = Some(ScopeBindings {
                all: bindings.all.clone(),
                rebindings: bindings.rebindings.clone(),
            });
            bindings.all.extend(node.binding_names().map(str::to_owned));
            bindings
                .rebindings
                .extend(node.rebinding_names().map(str::to_owned));
            bindings.all.sort();
            bindings.all.dedup();
            bindings.rebindings.sort();
            bindings.rebindings.dedup();
            preserve_invalid = merge_previous_scope_flow(context, state, node, flow);
        }

        let flow_is_empty = flow.is_empty();
        let mut fact_limit_exceeded = flow.fact_limit_exceeded;
        write_scope_bindings(node, state, bindings);
        if !preserve_invalid && !flow_is_empty {
            let boxed = prepared_flow.unwrap_or_else(|| Box::new(empty_flow));
            fact_limit_exceeded = write_scope_flow(node, context, state, boxed);
        } else if !preserve_invalid && reserved_flow_slot {
            set_optional_json_detail(node, NodeDetailKey::VALUE_FLOW, None);
        }

        if !node.details.is_object() {
            node.details = json!({});
        }
        if let Some((cognitive_complexity, max_nesting_depth, statements)) = metrics {
            if cognitive_complexity > 0 {
                node.set_detail(NodeDetailKey::COGNITIVE_COMPLEXITY, json!(cognitive_complexity));
            }
            if max_nesting_depth > 0 {
                node.set_detail(NodeDetailKey::MAX_NESTING_DEPTH, json!(max_nesting_depth));
            }
            if !statements.is_empty() {
                facts.statement_fingerprints.push(CallableFingerprints {
                    node_id: node.id.clone(),
                    statements,
                });
            }
        }
        if fact_limit_exceeded {
            node.set_detail(NodeDetailKey::FACT_LIMIT_EXCEEDED, json!(true));
        }
        if let Some(exports) = visibility_exports.as_deref() {
            context
                .profile
                .finalize_file_visibility(root, context.source, facts, exports);
        }
    }
}

fn module_export_enter_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "class_definition"
            | "lambda"
            | "assignment"
            | "augmented_assignment"
            | "delete_statement"
            | "import_statement"
            | "import_from_statement"
            | "for_statement"
            | "named_expression"
            | "as_pattern"
    )
}

fn module_export_exit_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition" | "class_definition" | "lambda"
    )
}

fn scope_boundary_kind(language: &str, kind: &str) -> bool {
    match language {
        "python" => matches!(kind, "function_definition" | "class_definition" | "lambda"),
        "rust" => matches!(
            kind,
            "function_item"
                | "function_signature_item"
                | "closure_expression"
                | "impl_item"
                | "trait_item"
                | "mod_item"
        ),
        "javascript" | "typescript" | "vue" => matches!(
            kind,
            "function_declaration"
                | "function_expression"
                | "generator_function_declaration"
                | "generator_function"
                | "generator_function_expression"
                | "arrow_function"
                | "method_definition"
                | "method_declaration"
                | "class_declaration"
                | "class"
        ),
        _ => binding_scope_kind(kind),
    }
}

fn syntax_doc(context: &VisitContext<'_>, node: Syntax<'_>) -> String {
    let doc = context.profile.doc_comment(node, context.source);
    if !doc.is_empty() {
        return doc;
    }
    if let Some(syntax) = &context.syntax {
        node.prev_named_sibling()
            .filter(|comment| {
                syntax
                    .classify(comment.kind_id())
                    .is_some_and(|kind| kind.is_comment())
            })
            .map(|comment| text(comment, context.source).to_owned())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

#[cfg(all(test, feature = "lang-python"))]
mod tree_cursor_walk_tests {
    use super::*;
    use crate::ast::syntax::walk_in_order;

    fn reference_walk<'tree>(
        node: Syntax<'tree>,
        reversed: bool,
        events: &mut Vec<(String, usize, usize, bool)>,
    ) {
        events.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            true,
        ));
        let mut children: Vec<_> = node.named_children(&mut node.walk()).collect();
        if reversed {
            children.reverse();
        }
        for child in children {
            reference_walk(child, reversed, events);
        }
        events.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            false,
        ));
    }

    #[test]
    fn tree_cursor_walk_preserves_named_node_events_in_both_directions() {
        let source =
            "import os\nx = call(1, [2, 3])\ndef f(a):\n    if a:\n        return {'x': a}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::languages::python::language())
            .expect("Python grammar");
        let tree = parser.parse(source, None).expect("Python syntax tree");
        let root = tree.root_node();

        for reversed in [false, true] {
            let mut expected = Vec::new();
            reference_walk(root, reversed, &mut expected);
            let mut actual = Vec::new();
            walk_in_order(root, reversed, &mut |node, entering| {
                actual.push((
                    node.kind().to_owned(),
                    node.start_byte(),
                    node.end_byte(),
                    entering,
                ));
            });
            assert_eq!(actual, expected, "reversed={reversed}");
            let mut actual_with_ancestors = Vec::new();
            walk_in_order_with_ancestors(root, reversed, &mut |node, ancestors, entering| {
                assert_eq!(ancestors.last().copied(), node.parent());
                actual_with_ancestors.push((
                    node.kind().to_owned(),
                    node.start_byte(),
                    node.end_byte(),
                    entering,
                ));
            });
            assert_eq!(actual_with_ancestors, expected, "ancestors reversed={reversed}");
        }
    }
}
