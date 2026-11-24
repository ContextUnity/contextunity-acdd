use crate::interner::LinkerInterner;
use crate::languages::{
    dependency_registry::DependencyRegistry,
    profile::LanguageProfile,
    python::common::normalize_python_import,
    types::{self as languages, ImportPath, LanguageFamily},
};
use crate::linker::traits::{
    insert_package_export, package_export, resolve_default_import, BuiltinPropertyLookup,
    ImportContext, ImportResolution, LanguageLinker, ModulesByNamespace, PackageExports,
};
use crate::linker::types::{qualified, NodeBindingIndex};
use crate::linker::value_flow::types::TypeTarget;
use crate::models::{
    tags::{EdgeKind, LanguageId, NodeKind},
    Facts, Node, Reference,
};
use hashbrown::{HashMap, HashSet};
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) struct PythonLinker;

pub(crate) static PYTHON_LINKER: PythonLinker = PythonLinker;

fn prefer_live_declaration(candidates: &mut Vec<&Node>) -> bool {
    let latest_assignment = candidates
        .iter()
        .filter(|node| node.kind == crate::models::tags::NodeKind::Variable)
        .map(|node| node.line)
        .max();
    let latest_declaration = candidates
        .iter()
        .filter(|node| node.kind != crate::models::tags::NodeKind::Variable)
        .map(|node| node.line)
        .max();
    if let (Some(assignment), Some(declaration)) = (latest_assignment, latest_declaration) {
        if assignment > declaration {
            candidates.clear();
            return true;
        } else {
            candidates.retain(|node| node.kind != crate::models::tags::NodeKind::Variable);
        }
    }
    false
}

impl LanguageLinker for PythonLinker {
    fn resolve_super_member<'a>(
        &self,
        context: &crate::linker::semantic_context::context::Context<'a>,
        owner: &Node,
        member: &str,
    ) -> crate::linker::value_flow::Symbol<'a> {
        context.python_super_member(owner, member)
    }
    fn special_member_call<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        receiver: &crate::semantic::ValueExpr,
        member: &str,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::{Symbol, TypeTarget};
        if !matches!(receiver, crate::semantic::ValueExpr::Call { callee, site: None } if callee == "super") {
            return None;
        }
        Some(match eval.resolver.resolve_super_member(eval.owner, member) {
            Symbol::Callable(callable) => eval.index.return_type_tracked(callable, eval.trace).cloned().unwrap_or(TypeTarget::Unknown),
            _ => TypeTarget::Unknown,
        })
    }
    fn accept_member_return(&self, member: &str, result: &crate::linker::value_flow::TypeTarget<'_>) -> bool {
        !(member == "__aenter__" && matches!(result, crate::linker::value_flow::TypeTarget::Unknown))
    }
    fn indirect_call_result<'a>(&self, eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>, callee: &str) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        let inner = callee.strip_suffix("()")?;
        Some(eval.index.type_of_expression_tracked(eval.owner, inner, eval.at, eval.resolver, eval.trace)
            .and_then(|target| super::value_flow::callable_target_return(eval.index, target, eval.owner, eval.at, eval.resolver, eval.trace))
            .cloned().unwrap_or(crate::linker::value_flow::TypeTarget::Unknown))
    }
    fn inherits_scope(&self, node: &Node, parent: &Node) -> bool {
        !(matches!(node.kind.as_str(), "function" | "method") && parent.kind == crate::models::tags::NodeKind::Class)
    }
    fn scoped_binding_restores(&self) -> bool { false }
    fn collection_return(&self, target: &crate::linker::value_flow::TypeTarget<'_>) -> bool {
        matches!(target, crate::linker::value_flow::TypeTarget::Builtin(name) if matches!(name.as_str(), "list" | "List" | "Sequence" | "Iterable"))
    }
    fn framework_magic_diagnostic(&self, profile: &dyn crate::languages::LanguageProfile, module: &str) -> bool { !profile.is_stdlib(module) }
    fn chained_member_fallback(&self) -> bool { true }
    fn computed_receiver<'a, 'b>(&self, query: &mut crate::linker::traits::DynamicReceiver<'a, 'b, '_>) -> (crate::languages::profile::LanguageMemberResult<'b>, bool) where 'a: 'b {
        let members = crate::linker::semantic_context::LanguageMemberLookup::new(query.visibility.input.semantic_context);
        let view = super::resolution::PythonResolutionView {
            input: query.visibility.input, path: query.visibility.path, file_module: query.file_module,
            aliases: query.visibility.aliases, alias_import_positions: query.visibility.alias_import_positions,
            member_lookup: &members,
        };
        let (result, builtin) = super::resolution::dynamic_receiver_member(&view, query.reference, query.constructor_aliases, query.candidates, query.lookup,
            |name, owner, definition| query.resolver.alias_for(name, owner, definition),
            |name, owner, definition| query.resolver.external_for(name, owner, definition));
        (result.into(), builtin)
    }
    fn implicit_receiver_member<'a>(&self, query: &mut crate::linker::traits::ImplicitReceiver<'a, '_, '_>) -> Option<crate::languages::profile::LanguageMemberResult<'a>> {
        let members = crate::linker::semantic_context::LanguageMemberLookup::new(query.phase.input.semantic_context);
        let view = super::resolution::PythonResolutionView {
            input: query.phase.input, path: query.phase.path, file_module: query.phase.file_module,
            aliases: query.visibility.aliases, alias_import_positions: query.visibility.alias_import_positions,
            member_lookup: &members,
        };
        super::resolution::implicit_receiver_member(&view, query.phase.reference,
            query.typed_receiver.map(crate::linker::types::ReceiverType::Local), query.known_receiver,
            query.shadowed, query.imported, query.lookup).map(Into::into)
    }
    fn imported_external_member<'a>(&self, phase: &crate::linker::traits::ReferencePhase<'a, '_>, module: &'a Node, member: &str, lookup: &mut String) -> Option<crate::linker::traits::ExternalMember<'a>> {
        let members = crate::linker::semantic_context::LanguageMemberLookup::new(phase.input.semantic_context);
        match super::resolution::imported_module_member(phase.profile, module, member, phase.input.node_rebindings, phase.input.interner, phase.input.by_qual, lookup, &members) {
            Some(super::receivers::PythonReceiverMember::External(origin, line, provider, _)) => Some((origin, line, provider)),
            _ => None,
        }
    }
    fn import_visible_at(&self, context: &crate::linker::traits::ImportVisibility<'_, '_, '_>, reference: &crate::models::Reference, definition_scope: bool) -> bool {
        super::resolution::import_visible_at(context, reference, definition_scope)
    }
    fn callable_members(&self) -> Option<(&'static str, &'static str)> {
        Some(("__init__", "__call__"))
    }
    fn typed_callable_evidence(&self) -> &'static str {
        "invocation of a typed Python Callable binding"
    }

    fn has_protocol_or_abc_base(&self, candidate: &Node) -> bool {
        crate::linker::python::receivers::has_protocol_or_abc_base(candidate)
    }

    fn class_type_uses_associated_lookup(&self) -> bool { true }

    fn supports_collection_facts(&self) -> bool { true }
    fn builtin_type_members(&self) -> bool { true }
    fn nullable_parameter_wrapper(&self, profile: &dyn crate::languages::LanguageProfile, annotation: &str, node: &Node, references: &[crate::models::Reference], external_line: &dyn Fn(&str, &Node) -> Option<usize>) -> bool {
        super::resolution::nullable_parameter_wrapper(Some(profile), annotation, node, references, external_line)
    }
    fn external_factory_result<'a>(&self, context: &crate::linker::semantic_context::context::Context<'a>, owner: &'a Node, callee: &str, at: crate::semantic::SourcePosition) -> Option<TypeTarget<'a>> {
        super::semantic_context::external_factory_result(context, owner, callee, at)
    }
    fn receiver_access(&self, owner: &Node, head: &str) -> Option<(bool, bool)> {
        super::semantic_context::receiver_access(owner, head)
    }
    fn external_import_type<'a>(&self, owner: &Node, module: &str, reference: &crate::models::Reference, (head, member): (&str, &str), role: crate::linker::value_flow::SymbolRole, line: usize) -> Option<crate::linker::value_flow::Symbol<'a>> {
        if matches!(module, "typing" | "typing_extensions") && (member == "Any" || head == "Any") {
            return Some(crate::linker::value_flow::Symbol::Unknown);
        }
        super::semantic_context::external_import_type(owner, module, reference, head, member, role, line)
    }
    fn external_generic_base(&self, module: &str, base: &str, profile: &dyn crate::languages::LanguageProfile) -> bool {
        super::semantic_context::is_external_generic_base(module, base, Some(profile))
    }

    fn resolve_applied_type<'a>(&self, context: &crate::linker::semantic_context::context::Context<'a>, owner: &'a Node, (base, args): (&str, &[crate::semantic::TypeExpr]), nominal: &crate::linker::value_flow::Symbol<'a>, at: crate::semantic::SourcePosition) -> Option<TypeTarget<'a>> {
        let clean_base = base.rsplit_once('.').map_or(base, |(_, name)| name);
        if matches!(clean_base, "tuple" | "Tuple") && !args.is_empty()
            && !args.iter().any(|arg| matches!(arg, crate::semantic::TypeExpr::Named { name } if name == "...")) {
            return Some(TypeTarget::Tuple(args.iter().map(|arg| crate::linker::value_flow::resolve_type(owner, arg, at, context, crate::linker::value_flow::SemanticLimits::default())).collect()));
        }
        super::semantic_context::resolve_applied(context, owner, base, args, nominal, at)
    }

    fn framework_member_label(&self) -> &'static str { "finite Python framework member" }

    fn external_member_policy<'a>(&self, owner: &Node, module: &'a str, line: usize, member: &str) -> Option<crate::languages::profile::LanguageMemberResult<'a>> {
        use super::semantic_context::ExternalMemberOutcome;
        use crate::languages::profile::LanguageMemberResult;
        Some(match super::semantic_context::external_member(owner, module, member)? {
            ExternalMemberOutcome::Supported(origin) => LanguageMemberResult::External { module, import_line: line, provider: Some(origin), hierarchy: None },
            ExternalMemberOutcome::Unsupported => LanguageMemberResult::Unknown,
        })
    }

    fn path_constructor_aliases<'a>(&self, profile: &dyn crate::languages::LanguageProfile, references: &'a [crate::models::Reference], by_id: &hashbrown::HashMap<crate::interner::SymbolId, &'a Node>, interner: &crate::interner::LinkerInterner<'_>, module: &'a str) -> hashbrown::HashMap<(&'a str, &'a str), &'a crate::models::Reference> {
        super::resolution::path_constructor_aliases(Some(profile), references, by_id, interner, module)
    }
    fn dynamic_type_aliases<'a>(&self, profile: &dyn crate::languages::LanguageProfile, references: &'a [crate::models::Reference], by_id: &hashbrown::HashMap<crate::interner::SymbolId, &'a Node>, interner: &crate::interner::LinkerInterner<'_>, module: &'a str) -> hashbrown::HashMap<&'a str, hashbrown::HashSet<&'a str>> {
        super::resolution::dynamic_type_aliases(Some(profile), references, by_id, interner, module)
    }

    fn supports_async_return(&self, owner: &Node) -> bool {
        matches!(owner.kind, crate::models::tags::NodeKind::Function | crate::models::tags::NodeKind::Method)
    }

    fn collection_return_element<'a>(
        &self,
        owner: &'a Node,
        expression: &crate::semantic::TypeExpr,
        resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
        limits: crate::linker::value_flow::SemanticLimits,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::python::value_flow::collection_return_element(owner, expression, resolver, limits)
    }

    fn builtin_evidence(&self, _first: &str, expression: &str) -> Option<(&'static str, String)> {
        Some((
            "Python standard library",
            format!("Python standard library built-in: {expression}"),
        ))
    }

    fn builtin_origin(&self) -> Option<&'static str> {
        Some("Python standard library")
    }

    fn uses_hierarchy_rules(&self) -> bool {
        true
    }

    fn constructor_result<'a>(
        &self,
        owner: &'a Node,
        callee: &str,
        at: crate::semantic::SourcePosition,
        resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::python::value_flow::constructor_result(owner, callee, at, resolver)
    }

    fn construct_value<'a>(&self, owner: &'a Node, callee: &str) -> Option<TypeTarget<'a>> {
        crate::linker::python::value_flow::literal_type(owner, callee)
    }

    fn evaluate_dotted_call<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        callee: &str,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::python::value_flow::dotted_member_result(
            eval.index,
            eval.owner,
            callee,
            eval.at,
            eval.resolver,
            eval.trace,
        )
    }

    fn evaluate_member_call_fallback<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        receiver: &TypeTarget<'a>,
        callee: &str,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::python::value_flow::synthetic_member_result(
            eval.index,
            eval.owner,
            receiver,
            callee,
            eval.at,
            eval.resolver,
            eval.trace,
        )
    }

    fn synthetic_property_result<'a>(
        &self,
        owner: &'a Node,
        receiver: &TypeTarget<'a>,
        member: &str,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::python::value_flow::synthetic_property_result(owner, receiver, member)
    }

    fn resolve_builtin_property<'a>(
        &self,
        _path: &str,
        receiver_name: &str,
        field: &str,
        _semantic_context: &dyn BuiltinPropertyLookup,
    ) -> Option<TypeTarget<'a>> {
        (receiver_name.starts_with("pathlib.") && field == "parent")
            .then(|| TypeTarget::Builtin(receiver_name.to_owned()))
    }

    fn builtin_member_origin(
        &self,
        receiver: &TypeTarget<'_>,
        default_origin: Option<&'static str>,
        computed_call: bool,
    ) -> Option<&'static str> {
        if computed_call {
            return default_origin;
        }
        match receiver {
            TypeTarget::Builtin(name) => match name.as_str() {
                "logging.Logger" => Some("builtin:logging.Logger"),
                "logging.LoggerAdapter" => Some("builtin:logging.LoggerAdapter"),
                "sqlite3.Connection" | "sqlite3.Cursor" => Some("builtin:sqlite3"),
                _ => default_origin,
            },
            _ => default_origin,
        }
    }

    fn compose_file_relations<'a>(
        &self,
        ctx: &mut crate::linker::traits::FileRelationContext<'_, 'a>,
    ) {
        crate::linker::python::semantic_relations::compose_symbol_relations(
            ctx.path,
            ctx.facts,
            ctx.references,
            ctx.dependencies,
            ctx.semantic_context,
            ctx.by_module,
            ctx.by_id,
            ctx.prepared_semantic_facts,
            ctx.graph,
        );
    }

    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        let paired_stub = prefer_runtime_module(context.modules);
        let mut result = resolve_default_import(context);
        result.paired_stub = paired_stub;
        let Some(tail) = context.tail().map(str::to_owned) else {
            return result;
        };
        if tail.is_empty() {
            return result;
        }
        if context.modules.is_empty() {
            let Some(normalized) = context.normalized else {
                return result;
            };
            let child_namespace = format!("{}.{}", normalized.namespace, tail);
            let workspace = languages::workspace_path(context.path).0;
            context.modules.extend(
                context
                    .modules_by_namespace
                    .get(&context.family)
                    .and_then(|namespaces| namespaces.get(&child_namespace))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| languages::workspace_path(&node.path).0 == workspace),
            );
            if let Some(stub) = prefer_runtime_module(context.modules) {
                result.paired_stub = Some(stub);
            }
            context.candidates.extend(context.modules.iter().copied());
            result.child_module = context.candidates.len() == 1;
            return result;
        }
        let Some(module) = context
            .modules
            .first()
            .copied()
            .filter(|_| context.modules.len() == 1)
        else {
            return result;
        };
        let export = package_export(
            context.package_exports,
            context.interner,
            module.path.as_str(),
            &tail,
        );
        if let Some(export) = export {
            if context
                .candidates
                .iter()
                .all(|candidate| candidate.kind == crate::models::tags::NodeKind::Variable)
            {
                context.candidates.clear();
                if let Some(target) = export {
                    context.candidates.push(target);
                    result.reexport_symbol = true;
                }
                return result;
            }
        }
        let blocked_assignment = prefer_live_declaration(context.candidates);
        if blocked_assignment {
            return result;
        }
        if context.candidates.is_empty() {
            let child_namespace = format!("{}.{}", context.selected_namespace, tail);
            let parent_workspace = languages::workspace_path(&module.path).0;
            context.candidates.extend(
                context
                    .modules_by_namespace
                    .get(&context.family)
                    .and_then(|namespaces| namespaces.get(&child_namespace))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| languages::workspace_path(&node.path).0 == parent_workspace),
            );
            if let Some(stub) = prefer_runtime_module(context.candidates) {
                result.paired_stub = Some(stub);
            }
            result.child_module = context.candidates.len() == 1;
        }
        if context.candidates.is_empty() {
            if let Some(stub) = result.paired_stub {
                context.lookup_key.clear();
                context.lookup_key.push_str(&stub.qualname);
                context.lookup_key.push('.');
                context.lookup_key.push_str(&tail);
                let target = context.lookup_key.as_str();
                context.candidates.extend(
                    context
                        .by_module
                        .get(stub.path.as_str())
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|node| node.kind != crate::models::tags::NodeKind::Component && node.qualname == target),
                );
                result.stub_symbol = !context.candidates.is_empty();
            }
        }
        if context.candidates.is_empty() {
            if let Some(target) =
                resolve_export(context.package_exports, context.interner, module, &tail)
            {
                context.candidates.push(target);
                result.reexport_symbol = true;
            }
        }
        result
    }

    fn package_exports<'a>(
        &self,
        interner: &mut LinkerInterner<'a>,
        node_rebindings: &NodeBindingIndex,
        all: &'a dyn crate::models::FactsLookup,
        modules_by_namespace: &crate::linker::traits::ModulesByNamespace<'a>,
        by_module: &HashMap<&'a str, Vec<&'a Node>>,
        root: Option<&Path>,
    ) -> crate::linker::traits::PackageExports<'a> {
        package_exports(
            interner,
            node_rebindings,
            all.values_facts(),
            modules_by_namespace,
            by_module,
            root,
        )
    }

    fn required_full_facts(
        &self,
        facts: &dyn crate::models::FactsLookup,
        affected: &BTreeSet<String>,
        catalog: &[(&str, &str)],
    ) -> BTreeSet<String> {
        required_full_facts(facts, affected, catalog)
    }

    fn needs_reference_identity(&self, path: &str, facts: &Facts) -> bool {
        needs_reference_identity(path, facts)
    }

    fn reference_identity_changed(&self, path: &str, old: &Facts, new: &Facts) -> bool {
        reference_identity_changed(path, old, new)
    }

    fn is_declaration_only(&self, path: &str) -> bool {
        Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "pyi")
    }

    fn resolve_imported_member<'a>(
        &self,
        interner: &LinkerInterner<'a>,
        module: &'a Node,
        member: &str,
        by_module: &HashMap<&'a str, Vec<&'a Node>>,
        by_qual: &HashMap<&'a str, Vec<&'a Node>>,
        exports: &crate::linker::traits::PackageExports<'a>,
        lookup_key: &mut String,
    ) -> Vec<&'a Node> {
        if Path::new(&module.path)
            .extension()
            .is_none_or(|extension| extension != "py")
        {
            return Vec::new();
        }
        let stub_path = format!("{}i", module.path);
        let Some(stub) = by_module
            .get(stub_path.as_str())
            .into_iter()
            .flatten()
            .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.qualname == module.qualname)
        else {
            return resolve_export(exports, interner, module, member)
                .into_iter()
                .collect();
        };
        lookup_key.clear();
        lookup_key.push_str(&stub.qualname);
        lookup_key.push('.');
        lookup_key.push_str(member);
        let target = lookup_key.as_str();
        let mut candidates: Vec<_> = by_qual
            .get(target)
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| node.path == stub.path)
            .collect();
        if candidates.is_empty() {
            candidates.extend(resolve_export(exports, interner, module, member));
        }
        candidates
    }

    fn emits_single_imported_target_edge(&self) -> bool { true }
    fn type_only_import_is_unproven(&self, profile: &dyn crate::languages::LanguageProfile, path: &str, facts: &Facts, modules: &crate::linker::traits::ModulesByNamespace<'_>, dependencies: &DependencyRegistry, interner: &LinkerInterner<'_>, node_rebindings: &NodeBindingIndex) -> bool {
        super::semantic_context::type_only_import_is_unproven(profile, path, facts, modules, dependencies, interner, node_rebindings)
    }
    fn is_dynamic_typing_annotation(&self, head: &str, member: &str, owner: &Node, aliases: &HashMap<&str, HashMap<&str, Vec<&Node>>>, dynamic_aliases: &HashMap<&str, hashbrown::HashSet<&str>>) -> bool {
        super::resolution::is_dynamic_typing_annotation(head, member, owner, aliases, dynamic_aliases)
    }
    fn is_dynamic_type_alias(&self, module: &str, expression: &str) -> bool {
        super::semantic_context::dynamic_type_alias("python", module, expression)
    }
    fn external_factory_import_is_safe(&self, module: &Node, alias: &str, position: crate::semantic::SourcePosition) -> bool {
        super::semantic_context::logging_factory_import_is_safe(module, alias, position)
    }
    fn special_type_alias_target<'a>(&self, node: &'a Node, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) -> Option<TypeTarget<'a>> {
        (node.kind == crate::models::tags::NodeKind::Class && typed_dict_class(node, resolver)).then(|| TypeTarget::Builtin("Mapping".to_owned()))
    }
    fn stored_type_expr(&self, annotation: &str) -> Option<crate::semantic::TypeExpr> {
        stored_python_type_expr(annotation)
    }
    fn callable_target_return<'s, 'a>(&self, index: &'s (dyn crate::linker::value_flow::types::ValueFlowLookup<'a> + 'a), target: &'s TypeTarget<'a>, owner: &'a Node, at: crate::semantic::SourcePosition, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>, trace: Option<&crate::linker::value_flow::types::ReadTrace<'a, '_>>) -> Option<&'s TypeTarget<'a>> {
        let TypeTarget::Local(class) = target else { return None; };
        if class.kind != crate::models::tags::NodeKind::Class { return None; }
        match resolver.resolve_member(target, "__call__", owner, at) {
            crate::linker::value_flow::Symbol::Callable(call_fn) => index.return_type_tracked(call_fn, trace),
            _ => None,
        }
    }
    fn constructor_symbol<'a>(&self, owner: &'a Node, expression: &str, at: crate::semantic::SourcePosition, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) -> Option<crate::linker::value_flow::Symbol<'a>> {
        super::value_flow::constructor_symbol(owner, expression, at, resolver)
    }
    fn collects_module_flow_rebindings(&self, node: &Node) -> bool { node.kind == crate::models::tags::NodeKind::Module }
    fn prefers_construct_contract(&self) -> bool { true }
    fn enforces_local_contract_position(&self) -> bool { true }
}

fn typed_dict_class<'a>(
    node: &'a Node,
    resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
) -> bool {
    use crate::linker::value_flow::{node_position, Symbol, SymbolRole, TypeTarget};
    let Some(bases) = node
        .detail_entry(crate::models::details::NodeDetailKey::BASES)
        .as_str()
        .and_then(|bases| bases.strip_prefix('('))
        .and_then(|bases| bases.strip_suffix(')'))
    else {
        return false;
    };
    let (base, options) = bases
        .split_once(',')
        .map_or((bases, ""), |(base, options)| (base, options));
    if !options.is_empty()
        && !options.split_once('=').is_some_and(|(key, value)| {
            key.trim() == "total" && matches!(value.trim(), "False" | "True")
        })
    {
        return false;
    }
    let base = base.trim();
    if !matches!(
        base,
        "TypedDict" | "typing.TypedDict" | "typing_extensions.TypedDict"
    ) {
        return false;
    }
    matches!(
        resolver.resolve_symbol(node, base, node_position(node), SymbolRole::Type),
        Symbol::Type(TypeTarget::External { module, .. })
            if matches!(module.as_str(), "typing" | "typing_extensions")
    )
}

fn stored_python_type_argument(annotation: &str) -> crate::semantic::TypeExpr {
    let unparenthesized = crate::ast::syntax::strip_outer_parentheses(annotation.trim());
    if unparenthesized.is_empty() {
        return crate::semantic::TypeExpr::Unknown;
    }
    stored_python_type_expr(unparenthesized).unwrap_or_else(|| crate::semantic::TypeExpr::Named {
        name: unparenthesized.to_owned(),
    })
}

fn stored_python_type_expr(annotation: &str) -> Option<crate::semantic::TypeExpr> {
    let annotation = crate::ast::syntax::strip_outer_parentheses(annotation.trim());
    #[cfg(feature = "lang-python")]
    let pipe_parts = crate::languages::python::common::split_python_pipe_types(annotation);
    #[cfg(not(feature = "lang-python"))]
    let pipe_parts: Option<Vec<&str>> = None;
    if let Some(parts) = pipe_parts {
        if parts.len() >= 2 {
            let args = parts.into_iter().map(stored_python_type_argument).collect();
            return Some(crate::semantic::TypeExpr::Applied {
                base: "Union".to_owned(),
                args,
            });
        }
    }
    let (base, rest) = annotation.split_once('[')?;
    let base = base.trim();
    let arguments = rest.strip_suffix(']')?;
    if base.is_empty() {
        return Some(crate::semantic::TypeExpr::Unknown);
    }
    let mut depth = 0_usize;
    let mut start = 0_usize;
    let mut args = Vec::new();
    for (index, character) in arguments.char_indices() {
        match character {
            '[' => depth = depth.checked_add(1)?,
            ']' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                let argument = arguments[start..index].trim();
                if argument.is_empty() {
                    return None;
                }
                args.push(stored_python_type_argument(argument));
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let argument = arguments[start..].trim();
    if argument.is_empty() {
        return None;
    }
    args.push(stored_python_type_argument(argument));
    Some(crate::semantic::TypeExpr::Applied {
        base: base.to_owned(),
        args,
    })
}

pub(super) fn needs_reference_identity(_path: &str, facts: &Facts) -> bool {
    facts
        .nodes
        .iter()
        .any(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Python)
}

pub(super) fn reference_identity_changed(path: &str, old: &Facts, new: &Facts) -> bool {
    if !needs_reference_identity(path, new) {
        return false;
    }
    let imports = |facts: &Facts| {
        let mut entries: Vec<_> = facts
            .references
            .iter()
            .filter(|reference| reference.kind == "imports")
            .map(|reference| {
                (
                    reference.source.clone(),
                    reference.expression.clone(),
                    reference.alias.clone(),
                    reference.module.clone(),
                )
            })
            .collect();
        entries.sort_unstable();
        entries
    };
    imports(old) != imports(new)
}

pub(super) fn required_full_facts(
    facts: &dyn crate::models::FactsLookup,
    affected: &BTreeSet<String>,
    catalog: &[(&str, &str)],
) -> BTreeSet<String> {
    let mut requested = HashMap::<&str, HashSet<String>>::new();
    let mut cross_workspace = HashSet::<String>::new();
    for (path, file) in facts.iter_facts() {
        let Some(module) = file
            .nodes
            .iter()
            .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Python)
        else {
            continue;
        };
        let workspace = languages::workspace_path(path).0;
        for reference in file.references.iter().filter(|reference| {
            reference.kind == "imports"
                && (affected.contains(path) || reference.source == module.id)
        }) {
            if let Some(import) = reference
                .module
                .as_deref()
                .and_then(|name| normalize_python_import(path, name))
            {
                let names = requested.entry(workspace).or_default();
                if reference.expression != "*"
                    && reference.module.as_deref() != Some(reference.expression.as_str())
                {
                    names.insert(format!("{}.{}", import.namespace, reference.expression));
                }
                if !import.relative && import.namespace.contains('.') {
                    cross_workspace.insert(import.namespace.clone());
                    if reference.expression != "*"
                        && reference.module.as_deref() != Some(reference.expression.as_str())
                    {
                        cross_workspace
                            .insert(format!("{}.{}", import.namespace, reference.expression));
                    }
                }
                names.insert(import.namespace);
            }
        }
        for export in module.detail_entry(crate::models::details::NodeDetailKey::LAZY_EXPORTS)
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(import) = lazy_import_path(module, export)
                .and_then(|name| normalize_python_import(path, &name))
            {
                if !import.relative
                    && export["relative_to_module"].as_bool() != Some(true)
                    && import.namespace.contains('.')
                {
                    cross_workspace.insert(import.namespace.clone());
                }
                requested
                    .entry(workspace)
                    .or_default()
                    .insert(import.namespace);
            }
        }
    }
    catalog
        .iter()
        .filter_map(|(path, language)| {
            if *language != "python" {
                return None;
            }
            let (workspace, local_path) = languages::workspace_path(path);
            let names = requested.get(workspace);
            let matches = |name: &str| {
                names.is_some_and(|names| names.contains(name)) || cross_workspace.contains(name)
            };
            let module_name = |p: &str| languages::module_stem(p).replace('/', ".");
            let full_name = module_name(path);
            let local_name = module_name(local_path);
            let src_name = local_path
                .split_once("/src/")
                .map(|(_, path)| module_name(path))
                .or_else(|| local_path.strip_prefix("src/").map(module_name));
            (matches(&full_name)
                || matches(&local_name)
                || src_name.as_ref().is_some_and(|name| matches(name)))
            .then(|| (*path).to_owned())
        })
        .collect()
}

pub(super) fn resolve_export<'a>(
    exports: &PackageExports<'a>,
    interner: &LinkerInterner<'_>,
    module: &Node,
    name: &str,
) -> Option<&'a Node> {
    package_export(exports, interner, module.path.as_str(), name).flatten()
}

fn node_rebound(
    module: &Node,
    name: &str,
    interner: &LinkerInterner<'_>,
    node_rebindings: &NodeBindingIndex,
) -> bool {
    module.rebinding_names().any(|candidate| candidate == name)
        || interner
            .lookup_symbol(&module.id)
            .zip(interner.lookup_symbol(name))
            .is_some_and(|(module_id, name_id)| {
                node_rebindings
                    .get(&module_id)
                    .is_some_and(|names| names.binary_search(&name_id).is_ok())
            })
}

fn nodes_by_qualname<'index, 'node>(
    by_qual: &'index HashMap<&'node str, Vec<&'node Node>>,
    qualname: &str,
) -> impl Iterator<Item = &'node Node> + 'index {
    by_qual.get(qualname).into_iter().flatten().copied()
}

fn scope_has_binding(
    by_qual: &HashMap<&str, Vec<&Node>>,
    interner: &LinkerInterner<'_>,
    index: &NodeBindingIndex,
    path: &str,
    scope: &str,
    head_id: crate::interner::SymbolId,
) -> bool {
    by_qual.get(scope).is_some_and(|nodes| {
        nodes.iter().any(|node| {
            node.path == path
                && interner
                    .lookup_symbol(&node.id)
                    .and_then(|node_id| index.get(&node_id))
                    .is_some_and(|bindings| bindings.binary_search(&head_id).is_ok())
        })
    })
}

fn collect_python_inheritance_imports<'a>(
    all: &'a (dyn crate::FactsLookup + Sync),
    interner: &LinkerInterner<'a>,
    by_id: &HashMap<crate::interner::SymbolId, &'a Node>,
) -> HashMap<(&'a str, &'a str, &'a str), Vec<&'a Reference>> {
    let mut inheritance_imports: HashMap<(&str, &str, &str), Vec<&Reference>> = HashMap::new();
    for (path, facts) in all.iter_facts() {
        if !facts.nodes.iter().any(|node| {
            node.language == LanguageId::Python && node.kind == NodeKind::Class
        }) {
            continue;
        }
        for import in facts
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            if let (Some(alias), Some(owner)) = (
                import.alias.as_deref(),
                interner.lookup_node(by_id, import.source.as_str()),
            ) {
                inheritance_imports
                    .entry((path, owner.qualname.as_str(), alias))
                    .or_default()
                    .push(import);
            }
        }
    }
    inheritance_imports
}

struct PythonBaseLookupCtx<'index, 'a> {
    interner: &'index LinkerInterner<'a>,
    by_qual: &'index HashMap<&'a str, Vec<&'a Node>>,
    node_bindings: &'index NodeBindingIndex,
    node_rebindings: &'index NodeBindingIndex,
    by_module: &'index HashMap<&'a str, Vec<&'a Node>>,
    profiles: &'index HashMap<&'a str, &'static dyn LanguageProfile>,
    normalized_imports: &'index HashMap<(&'a str, &'a str), Option<ImportPath>>,
    modules_by_namespace: &'index ModulesByNamespace<'a>,
    package_exports: &'index HashMap<&'static str, PackageExports<'a>>,
    dependency_registry: &'a DependencyRegistry,
    inheritance_imports: &'index HashMap<(&'a str, &'a str, &'a str), Vec<&'a Reference>>,
}

fn resolve_python_imported_base<'a>(
    ctx: &PythonBaseLookupCtx<'_, 'a>,
    class: &Node,
    import: &'a Reference,
    tail: &str,
    key: &mut String,
) -> crate::linker::python::receivers::Base<'a> {
    use crate::linker::python::receivers::Base;
    let Some(_profile) = ctx.profiles.get(class.path.as_str()).copied() else {
        return Base::Unknown;
    };
    let normalized = ctx
        .normalized_imports
        .get(&(
            class.path.as_str(),
            import.module.as_deref().unwrap_or(&import.expression),
        ))
        .and_then(Option::as_ref);
    let Some(normalized) = normalized else {
        return Base::Unknown;
    };
    let mut modules: Vec<_> = ctx
        .modules_by_namespace
        .get(&LanguageFamily("python"))
        .and_then(|all_namespaces| all_namespaces.get(&normalized.namespace))
        .into_iter()
        .flatten()
        .copied()
        .collect();
    let workspace = languages::workspace_path(&class.path).0;
    if modules
        .iter()
        .any(|module| languages::workspace_path(&module.path).0 == workspace)
    {
        modules.retain(|module| languages::workspace_path(&module.path).0 == workspace);
    } else if normalized.relative {
        modules.clear();
    }
    let mut targets = Vec::new();
    PYTHON_LINKER.resolve_import(&mut ImportContext {
        path: &class.path,
        interner: ctx.interner,
        dependencies: ctx.dependency_registry,
        reference: import,
        language_state: None,
        normalized: Some(normalized),
        selected_namespace: &normalized.namespace,
        family: LanguageFamily("python"),
        modules: &mut modules,
        candidates: &mut targets,
        modules_by_namespace: ctx.modules_by_namespace,
        by_module: ctx.by_module,
        by_qual: ctx.by_qual,
        package_exports: &ctx.package_exports["python"],
        lookup_key: key,
    });
    if !tail.is_empty() {
        targets = targets
            .iter()
            .flat_map(|target| {
                nodes_by_qualname(ctx.by_qual, &format!("{}.{}", target.qualname, tail))
                    .filter(move |node| node.path == target.path)
            })
            .collect();
    }
    targets.retain(|target| target.kind == NodeKind::Class);
    targets.sort_by_key(|target| &target.id);
    targets.dedup_by_key(|target| &target.id);
    if targets.len() == 1 {
        return Base::Local(&targets[0].id);
    }
    if targets.is_empty() && modules.is_empty() && !normalized.relative {
        let module = import.module.as_deref().unwrap_or(&import.expression);
        let mut canonical = module.to_owned();
        if import.expression != module {
            canonical.push('.');
            canonical.push_str(&import.expression);
        }
        if !tail.is_empty() {
            canonical.push('.');
            canonical.push_str(tail);
        }
        return Base::External(canonical, module, import.line);
    }
    Base::Unknown
}

fn resolve_python_base<'a>(
    ctx: &PythonBaseLookupCtx<'_, 'a>,
    class: &Node,
    reference: &Reference,
) -> crate::linker::python::receivers::Base<'a> {
    use crate::linker::python::receivers::Base;
    let expression = reference.expression.as_str();
    let (head, tail) = expression.split_once('.').unwrap_or((expression, ""));
    let Some(head_id) = ctx.interner.lookup_symbol(head) else {
        return Base::Unknown;
    };
    let mut scope = class
        .qualname
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    let mut key = String::new();
    while !scope.is_empty() {
        let imports = ctx
            .inheritance_imports
            .get(&(class.path.as_str(), scope, head))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if scope_has_binding(
            ctx.by_qual,
            ctx.interner,
            ctx.node_rebindings,
            &class.path,
            scope,
            head_id,
        ) {
            return Base::Unknown;
        }
        let local: Vec<_> = nodes_by_qualname(ctx.by_qual, qualified(&mut key, scope, expression))
            .filter(|node| node.path == class.path && node.kind == NodeKind::Class)
            .collect();
        if local.len() == 1 {
            if imports
                .iter()
                .any(|import| import.line > local[0].line && import.line <= class.line)
            {
                return Base::Unknown;
            }
            return Base::Local(&local[0].id);
        }
        if local.len() > 1 {
            return Base::Unknown;
        }
        if !imports.is_empty() {
            if imports.len() != 1 {
                return Base::Unknown;
            }
            let import = imports[0];
            if import.line > class.line
                || scope_has_binding(
                    ctx.by_qual,
                    ctx.interner,
                    ctx.node_rebindings,
                    &class.path,
                    scope,
                    head_id,
                )
            {
                return Base::Unknown;
            }
            return resolve_python_imported_base(ctx, class, import, tail, &mut key);
        }
        if scope_has_binding(
            ctx.by_qual,
            ctx.interner,
            ctx.node_bindings,
            &class.path,
            scope,
            head_id,
        ) {
            return Base::Unknown;
        }
        scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
    if expression == "object" {
        Base::External("builtins.object".into(), "builtins", 0)
    } else {
        Base::Unknown
    }
}

/// Builds Python class-hierarchy receiver lookups using indexed imports and workspace bindings.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_python_receivers<'index, 'a>(
    all: &'a (dyn crate::FactsLookup + Sync),
    interner: &LinkerInterner<'a>,
    by_id: &'index HashMap<crate::interner::SymbolId, &'a Node>,
    by_qual: &'index HashMap<&'a str, Vec<&'a Node>>,
    node_bindings: &'index NodeBindingIndex,
    node_rebindings: &'index NodeBindingIndex,
    by_module: &'index HashMap<&'a str, Vec<&'a Node>>,
    profiles: &'index HashMap<&'a str, &'static dyn LanguageProfile>,
    normalized_imports: &'index HashMap<(&'a str, &'a str), Option<ImportPath>>,
    modules_by_namespace: &'index ModulesByNamespace<'a>,
    package_exports: &'index HashMap<&'static str, PackageExports<'a>>,
    dependency_registry: &'a DependencyRegistry,
) -> crate::linker::python::receivers::PythonReceivers<'a> {
    use crate::linker::python::receivers::PythonReceivers;

    let inheritance_imports = collect_python_inheritance_imports(all, interner, by_id);
    let lookup_ctx = PythonBaseLookupCtx {
        interner,
        by_qual,
        node_bindings,
        node_rebindings,
        by_module,
        profiles,
        normalized_imports,
        modules_by_namespace,
        package_exports,
        dependency_registry,
        inheritance_imports: &inheritance_imports,
    };

    PythonReceivers::build(
        all,
        |class, reference| resolve_python_base(&lookup_ctx, class, reference),
        |path, base| {
            let (pack, rule) =
                dependency_registry.class_hierarchy_authority_for_path(path, base, None)?;
            Some((rule.provider.as_str(), pack, rule))
        },
    )
}

fn lazy_import_path(module: &Node, export: &serde_json::Value) -> Option<String> {
    let path = export["module"].as_str()?;
    if export["relative_to_module"].as_bool() != Some(true) || !path.starts_with('.') {
        return Some(path.to_owned());
    }
    let levels = path.bytes().take_while(|byte| *byte == b'.').count();
    let mut namespace = module.qualname.as_str();
    for _ in 1..levels {
        namespace = namespace.rsplit_once('.')?.0;
    }
    let tail = &path[levels..];
    Some(if tail.is_empty() {
        namespace.to_owned()
    } else {
        format!("{namespace}.{tail}")
    })
}

fn export_provider<'a>(
    path: &str,
    module_path: &str,
    modules: &HashMap<LanguageFamily, HashMap<String, Vec<&'a Node>>>,
    root: Option<&Path>,
) -> Option<&'a Node> {
    let _ = root;
    let import = normalize_python_import(path, module_path)?;
    let providers = modules
        .get(&LanguageFamily("python"))?
        .get(&import.namespace)?;
    let workspace = languages::workspace_path(path).0;
    let mut selected: Vec<_> = providers
        .iter()
        .copied()
        .filter(|provider| languages::workspace_path(&provider.path).0 == workspace)
        .collect();
    if selected.is_empty() && !import.relative && import.namespace.contains('.') {
        selected.extend(
            providers
                .iter()
                .copied()
                .filter(|provider| languages::workspace_path(&provider.path).0 != workspace),
        );
    }
    prefer_runtime_module(&mut selected);
    match selected.as_slice() {
        [provider] => Some(*provider),
        _ => None,
    }
}

struct PythonExportBinding<'a> {
    owner: &'a str,
    name: &'a str,
    provider: Option<&'a Node>,
    member: Option<&'a str>,
}

fn register_export<'a>(
    exports: &mut PackageExports<'a>,
    interner: &mut LinkerInterner<'a>,
    pending: &mut Vec<(&'a str, &'a str, &'a Node, &'a str)>,
    seen: &mut HashSet<(&'a str, &'a str)>,
    binding: PythonExportBinding<'a>,
    by_module: &HashMap<&str, Vec<&'a Node>>,
) {
    let PythonExportBinding {
        owner,
        name,
        provider,
        member,
    } = binding;
    if !seen.insert((owner, name)) {
        insert_package_export(exports, interner, owner, name, None);
        pending.retain(|(path, alias, _, _)| *path != owner || *alias != name);
        return;
    }
    let Some(provider) = provider else {
        return;
    };
    let path_id = interner.intern_path(owner);
    let name_id = interner.intern_symbol(name);
    let target = exports
        .entry(path_id)
        .or_default()
        .entry(name_id)
        .or_insert(None);
    let Some(member) = member else {
        *target = Some(provider);
        return;
    };
    let qualified = format!("{}.{}", provider.qualname, member);
    let mut candidates: Vec<&Node> = by_module
        .get(provider.path.as_str())
        .into_iter()
        .flatten()
        .copied()
        .filter(|node| node.kind != crate::models::tags::NodeKind::Component && node.qualname == qualified)
        .collect();
    let _ = prefer_live_declaration(&mut candidates);
    crate::linker::traits::filter_stubs_and_overloads(&mut candidates);
    if candidates.len() == 1
        && candidates[0].kind == crate::models::tags::NodeKind::Variable
        && provider.detail_entry(crate::models::details::NodeDetailKey::EXPORTS).as_array().is_some_and(|items| {
            items.iter().any(|export| {
                export["name"].as_str() == Some(member) && export["local"].as_str().is_some()
            })
        })
    {
        pending.push((owner, name, provider, member));
        return;
    }
    if candidates.len() == 1 {
        *target = Some(candidates[0]);
    } else if candidates.is_empty() {
        pending.push((owner, name, provider, member));
    }
}

pub(super) fn package_exports<'a>(
    interner: &mut LinkerInterner<'a>,
    node_rebindings: &NodeBindingIndex,
    all: impl IntoIterator<Item = &'a Facts>,
    modules_by_namespace: &HashMap<LanguageFamily, HashMap<String, Vec<&'a Node>>>,
    by_module: &HashMap<&str, Vec<&'a Node>>,
    root: Option<&Path>,
) -> PackageExports<'a> {
    let mut exports: PackageExports<'a> = HashMap::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::new();
    for facts in all {
        let Some(module) = facts
            .nodes
            .iter()
            .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Python)
        else {
            continue;
        };
        let path = module.path.as_str();
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == "imports" && reference.source == module.id)
        {
            let (Some(alias), Some(import_path)) =
                (reference.alias.as_deref(), reference.module.as_deref())
            else {
                continue;
            };
            if node_rebound(module, alias, interner, node_rebindings) {
                insert_package_export(&mut exports, interner, path, alias, None);
                continue;
            }
            if reference.expression == import_path
                && import_path.contains('.')
                && import_path.split('.').next() == Some(alias)
            {
                continue;
            }
            let mut provider = export_provider(path, import_path, modules_by_namespace, root);
            let mut member = (reference.module.as_deref() != Some(reference.expression.as_str()))
                .then_some(reference.expression.as_str());
            if let (Some(parent), Some(name)) = (provider, member) {
                let qualified = format!("{}.{}", parent.qualname, name);
                let declared = by_module
                    .get(parent.path.as_str())
                    .into_iter()
                    .flatten()
                    .any(|node| node.qualname == qualified && node.kind != crate::models::tags::NodeKind::Component);
                if !declared {
                    let child_path = format!(
                        "{import_path}{}{name}",
                        if import_path.ends_with('.') { "" } else { "." }
                    );
                    if let Some(child) =
                        export_provider(path, &child_path, modules_by_namespace, root)
                    {
                        provider = Some(child);
                        member = None;
                    }
                }
            }
            register_export(
                &mut exports,
                interner,
                &mut pending,
                &mut seen,
                PythonExportBinding {
                    owner: path,
                    name: alias,
                    provider,
                    member,
                },
                by_module,
            );
        }
        for export in module.detail_entry(crate::models::details::NodeDetailKey::EXPORTS).as_array().into_iter().flatten() {
            let Some(name) = export["name"].as_str() else {
                continue;
            };
            insert_package_export(&mut exports, interner, path, name, None);
            let provider = export["module"]
                .as_str()
                .and_then(|module_path| {
                    export_provider(path, module_path, modules_by_namespace, root)
                })
                .or_else(|| {
                    (export["local"].as_str().is_some() && export["module"].is_null())
                        .then_some(module)
                });
            register_export(
                &mut exports,
                interner,
                &mut pending,
                &mut seen,
                PythonExportBinding {
                    owner: path,
                    name,
                    provider,
                    member: export["local"].as_str(),
                },
                by_module,
            );
        }
        for export in module.detail_entry(crate::models::details::NodeDetailKey::LAZY_EXPORTS)
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(name) = export["name"].as_str() else {
                continue;
            };
            if seen.contains(&(path, name)) {
                continue;
            }
            if node_rebound(module, name, interner, node_rebindings) {
                continue;
            }
            let Some(import_path) = lazy_import_path(module, export) else {
                continue;
            };
            let provider = export_provider(path, &import_path, modules_by_namespace, root).filter(
                |provider| {
                    export["relative_to_module"].as_bool() != Some(true)
                        || languages::workspace_path(&provider.path).0
                            == languages::workspace_path(path).0
                },
            );
            register_export(
                &mut exports,
                interner,
                &mut pending,
                &mut seen,
                PythonExportBinding {
                    owner: path,
                    name,
                    provider,
                    member: export["member"].as_str(),
                },
                by_module,
            );
        }
    }
    loop {
        let before = pending.len();
        pending.retain(|(owner, name, provider, member)| {
            let target = resolve_export(&exports, interner, provider, member);
            if let Some(target) = target {
                insert_package_export(&mut exports, interner, owner, name, Some(target));
                false
            } else {
                true
            }
        });
        if pending.len() == before {
            break;
        }
    }
    exports
}

pub(super) fn prefer_runtime_module<'a>(modules: &mut Vec<&'a Node>) -> Option<&'a Node> {
    if modules.len() != 2 {
        return None;
    }
    let runtime = modules.iter().position(|node| node.path.ends_with(".py"));
    let stub = modules.iter().position(|node| node.path.ends_with(".pyi"));
    let (Some(runtime), Some(stub)) = (runtime, stub) else {
        return None;
    };
    let runtime_stem = modules[runtime].path.strip_suffix(".py");
    let stub_stem = modules[stub].path.strip_suffix(".pyi");
    if runtime_stem == stub_stem
        && languages::workspace_path(&modules[runtime].path).0
            == languages::workspace_path(&modules[stub].path).0
    {
        let stub_node = modules[stub];
        modules.swap(0, runtime);
        modules.truncate(1);
        return Some(stub_node);
    }
    None
}
