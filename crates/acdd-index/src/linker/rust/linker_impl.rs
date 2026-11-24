use super::forwarding::{
    crate_namespace, declared_exports, is_crate_visible, module_namespace, resolve_crate_namespace,
    within, WorkspaceCrateMatch,
};
use crate::interner::LinkerInterner;
use crate::languages::{
    dependency_registry::DependencyRegistry,
    profile::LanguageProfile,
    rust::visibility::{normalize_root_crate_import, rust_module_name},
    types::{self as languages, ImportPath, LanguageFamily},
};
use crate::linker::traits::{
    package_export, resolve_default_import, ImportContext, ImportNormalizationContext,
    ImportResolution, LanguageLinker, ModulesByNamespace, PackageExports,
};
use crate::linker::types::NodeBindingIndex;
use crate::models::{
    tags::{EdgeKind, LanguageId},
    Facts, Node,
};
use hashbrown::{HashMap, HashSet};

fn relative_import(namespace: String) -> Option<ImportPath> {
    Some(ImportPath {
        namespace,
        relative: true,
        symbol_path: true,
    })
}

fn normalize_declared_child_module(
    path: &str,
    module: &str,
    facts: &Facts,
    ctx: &ImportNormalizationContext<'_, '_>,
) -> Option<ImportPath> {
    let local_import = module.strip_prefix("self::").unwrap_or(module);
    let (first, tail) = local_import.split_once("::").unwrap_or((local_import, ""));
    let declared = facts.references.iter().any(|reference| {
        reference.kind == EdgeKind::Imports.as_str()
            && reference.alias.as_deref() == Some(first)
            && reference.module.as_deref() == Some(format!("self::{first}").as_str())
    });
    if !declared {
        return None;
    }
    let (workspace, local) = languages::workspace_path(path);
    let source = std::path::Path::new(local);
    let parent = source.parent().unwrap_or_else(|| std::path::Path::new(""));
    let integration_root = parent.file_name().is_some_and(|name| name == "tests");
    let directory = if integration_root
        || matches!(
            source.file_name().and_then(|name| name.to_str()),
            Some("lib.rs" | "main.rs" | "mod.rs" | "build.rs")
        )
        || parent.file_name().is_some_and(|name| name == "bin")
    {
        parent.to_path_buf()
    } else {
        source.with_extension("")
    };
    for candidate in [
        directory.join(format!("{first}.rs")),
        directory.join(first).join("mod.rs"),
    ] {
        let local = candidate.to_string_lossy().replace('\\', "/");
        let indexed = if workspace.is_empty() {
            local.clone()
        } else {
            format!("{workspace}/{local}")
        };
        if ctx
            .profiles
            .get(indexed.as_str())
            .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
        {
            let mut namespace = rust_module_name(&local);
            if !tail.is_empty() && tail != "*" {
                namespace.push('.');
                namespace.push_str(&tail.replace("::", "."));
            }
            return relative_import(namespace);
        }
    }
    None
}

fn normalize_single_rust_import(
    profile: &dyn LanguageProfile,
    path: &str,
    module: &str,
    facts: &Facts,
    ctx: &ImportNormalizationContext<'_, '_>,
    root_rust_crate: Option<&str>,
) -> Option<ImportPath> {
    if let Some(child_import) = normalize_declared_child_module(path, module, facts, ctx) {
        return Some(child_import);
    }
    let (first, tail) = module.split_once("::").unwrap_or((module, ""));
    match resolve_crate_namespace(
        ctx.profiles.keys().copied(),
        |candidate| ctx.profiles.contains_key(candidate),
        ctx.dependencies,
        path,
        first,
        tail,
    ) {
        WorkspaceCrateMatch::Resolved(namespace, _) => return relative_import(namespace),
        WorkspaceCrateMatch::Isolated => return None,
        WorkspaceCrateMatch::Missing => {}
    }
    if ctx
        .dependencies
        .rust_package_for_path(path)
        .is_some_and(|(_, own_lib)| own_lib == first)
        && !ctx.dependencies.is_rust_sibling_target_path(path)
        && !ctx.dependencies.declares_for_path(
            LanguageFamily(LanguageId::Rust.as_str()),
            path,
            first,
        )
    {
        return None;
    }
    if let Some(normalized) =
        root_rust_crate.and_then(|name| normalize_root_crate_import(name, module))
    {
        return Some(normalized);
    }
    profile.normalize_import_with_root(ctx.root, path, module)
}

pub(crate) struct RustLinker;
pub(crate) static RUST_LINKER: RustLinker = RustLinker;
impl LanguageLinker for RustLinker {
    fn incomplete_receiver(&self, receiver: &crate::linker::value_flow::TypeTarget<'_>) -> bool {
        matches!(receiver, crate::linker::value_flow::TypeTarget::RustReference { target, .. } if matches!(target.as_ref(), crate::linker::value_flow::TypeTarget::Unknown))
    }
    fn callback_receiver<'a>(&self, receiver: &crate::linker::value_flow::TypeTarget<'a>, method: &str) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::callback_receiver(receiver, method)
    }
    fn special_index_projection<'a>(&self, target: &crate::linker::value_flow::TypeTarget<'a>, index: &str) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::TypeTarget;
        match target {
            TypeTarget::RustTuple(items) => Some(index.parse::<usize>().ok().and_then(|index| items.get(index)).cloned().unwrap_or(TypeTarget::Unknown)),
            TypeTarget::RustCollection { item } | TypeTarget::RustSlice { item } | TypeTarget::RustTypedContainer { item, .. } => Some((**item).clone()),
            _ => None,
        }
    }
    fn special_field_projection<'a>(
        &self, target: &crate::linker::value_flow::TypeTarget<'a>, member: &str, owner: &'a Node,
        at: crate::semantic::SourcePosition, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::{Symbol, TypeTarget};
        match target {
            TypeTarget::RustTuple(_) => self.special_index_projection(target, member),
            TypeTarget::RustReference { .. } | TypeTarget::RustDeref { .. } => Some(match resolver.resolve_member(target, member, owner, at) {
                Symbol::Type(target) => target,
                Symbol::Callable(node) => TypeTarget::Callable(node),
                Symbol::Ambiguous => TypeTarget::Ambiguous,
                _ => TypeTarget::Unknown,
            }),
            _ => None,
        }
    }
    fn evaluate_language_value<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        value: &crate::semantic::ValueExpr,
        limits: crate::linker::value_flow::SemanticLimits,
        evaluate: &dyn Fn(&crate::semantic::ValueExpr) -> crate::linker::value_flow::TypeTarget<'a>,
        payload: &dyn Fn(&crate::semantic::ValueExpr) -> crate::linker::value_flow::TypeTarget<'a>,
        join: &dyn Fn(Vec<crate::linker::value_flow::TypeTarget<'a>>) -> crate::linker::value_flow::TypeTarget<'a>,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::evaluate_language_value(self, eval, value, limits, evaluate, payload, join)
    }
    fn join_language_targets<'a>(
        &self,
        targets: &[crate::linker::value_flow::TypeTarget<'a>],
        join: &dyn Fn(Vec<crate::linker::value_flow::TypeTarget<'a>>) -> crate::linker::value_flow::TypeTarget<'a>,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::join_targets(targets, join)
    }
    fn member_receiver<'b, 'a>(
        &self, receiver: &'b crate::linker::value_flow::TypeTarget<'a>, member: &str,
    ) -> Option<Option<&'b crate::linker::value_flow::TypeTarget<'a>>> {
        use crate::linker::value_flow::TypeTarget;
        match receiver {
            TypeTarget::RustReference { target, .. } => Some(Some(target)),
            TypeTarget::RustDeref { wrapper, target } => Some((!self.wrapper_value_member(wrapper, member)).then_some(target)),
            _ => None,
        }
    }
    fn special_named_return<'a>(
        &self,
        name: &str,
        import_line: usize,
        in_hierarchy: &dyn Fn() -> bool,
    ) -> Option<Option<crate::linker::value_flow::TypeTarget<'a>>> {
        use crate::linker::value_flow::TypeTarget;
        if !name.contains([':', '.']) && crate::languages::by_id("rust").is_some_and(|profile| profile.builtin_type(name)) {
            return Some(Some(TypeTarget::Builtin(name.to_owned())));
        }
        if !matches!(name.split("::").next(), Some("std" | "core" | "alloc"))
            && !name.starts_with("serde::ser::") && !in_hierarchy()
        {
            return Some(None);
        }
        Some(Some(TypeTarget::External { module: name.to_owned(), import_line }))
    }
    fn special_symbol<'a>(
        &self,
        context: &crate::linker::semantic_context::context::Context<'a>,
        owner: &'a Node,
        name: &str,
        at: crate::semantic::SourcePosition,
        role: crate::linker::value_flow::SymbolRole,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        context.resolve_rust_symbol(owner, name, at, role)
    }
    fn external_symbol<'a>(
        &self,
        context: &crate::linker::semantic_context::context::Context<'a>,
        owner: &'a Node,
        name: &str,
        role: crate::linker::value_flow::SymbolRole,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        context.resolve_rust_external_or_crate_symbol(owner, name, role)
    }
    fn imported_member_path<'a>(
        &self,
        context: &crate::linker::semantic_context::context::Context<'a>,
        owner: &'a Node,
        targets: &[&'a Node],
        member: &str,
        role: crate::linker::value_flow::SymbolRole,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        Some(context.resolve_rust_member_path(owner, targets, member, role))
    }
    fn special_member_result<'a>(
        &self,
        context: &crate::linker::semantic_context::context::Context<'a>,
        receiver: &crate::linker::value_flow::TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
    ) -> Option<Option<crate::linker::value_flow::TypeTarget<'a>>> {
        super::semantic_context::special_member_result(context, receiver, member, owner)
    }
    fn value_shadows_role(&self, name: &str, role: crate::linker::value_flow::SymbolRole) -> bool {
        !matches!(role, crate::linker::value_flow::SymbolRole::Type)
            && !name.split_once('.').map_or(name, |(before_dot, _)| before_dot).contains("::")
    }
    fn external_field_projection(&self) -> bool { true }
    fn reaching_payload_wrapper(&self, target: &crate::linker::value_flow::TypeTarget<'_>) -> bool {
        use crate::linker::value_flow::TypeTarget;
        matches!(target, TypeTarget::RustWrapper { .. })
            || matches!(target, TypeTarget::Builtin(name) if matches!(name.as_str(), "Option" | "Result"))
    }
    fn structural_operation_admitted<'a>(
        &self,
        owner: &'a Node,
        operation: crate::semantic::types::RustProjection,
        at: crate::semantic::SourcePosition,
        resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) -> bool {
        operation != crate::semantic::types::RustProjection::IdentitySome
            || resolver.unshadowed_global(owner, "Some", at)
    }
    fn lexical_receiver_field(&self, owner: &Node, receiver: &str) -> bool {
        receiver == "self" && owner.detail_entry(crate::models::details::NodeDetailKey::RECEIVER_NAME).as_str() == Some("self")
    }
    fn special_property_lookup<'s, 'a>(
        &self,
        context: &'s crate::linker::semantic_context::context::Context<'a>,
        receiver: &'s crate::linker::value_flow::TypeTarget<'a>,
        member: &str,
        caller: &'a Node,
    ) -> Option<crate::linker::semantic_context::context::Member<'s>> {
        super::semantic_context::special_property_lookup(context, receiver, member, caller)
    }
    fn special_member_lookup<'s, 'a>(
        &self,
        context: &'s crate::linker::semantic_context::context::Context<'a>,
        receiver: &'s crate::linker::value_flow::TypeTarget<'a>,
        member: &str,
        caller: &'a Node,
        associated: bool,
    ) -> Option<crate::linker::semantic_context::context::Member<'s>> {
        super::semantic_context::special_member_lookup(context, receiver, member, caller, associated)
    }
    fn member_call_result<'a>(
        &self,
        receiver: &crate::linker::value_flow::TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        at: crate::semantic::SourcePosition,
        resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::TypeTarget;
        match resolver.resolve_member_result(receiver, member, owner, at) {
            Some(target @ (TypeTarget::RustWrapper { .. } | TypeTarget::RustIterator { .. } | TypeTarget::RustTuple(_))) => Some(target),
            _ => self.declared_adapter_value(receiver, member, owner, at, resolver),
        }
    }
    fn project_fields<'a>(&self, fields: &mut hashbrown::HashMap<&'a str, hashbrown::HashMap<String, crate::linker::value_flow::TypeTarget<'a>>>, owners: &[&'a Node], resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) {
        super::value_flow::project_deref_fields(fields, owners, resolver);
    }
    fn receiver_symbol<'a>(&self, lookup: &crate::linker::semantic_context::LanguageMemberLookup<'_, 'a>, owner: &'a Node, name: &str, resolve_type: &mut dyn FnMut(&str) -> crate::linker::value_flow::Symbol<'a>) -> Option<crate::linker::value_flow::Symbol<'a>> {
        super::semantic_context::receiver_symbol(Some(lookup), owner, name, resolve_type)
    }
    fn project_reference<'a>(&self, target: crate::linker::value_flow::TypeTarget<'a>, mutable: bool) -> crate::linker::value_flow::TypeTarget<'a> {
        crate::linker::value_flow::TypeTarget::RustReference { mutable, target: Box::new(target) }
    }
    fn project_tuple<'a>(&self, items: Vec<crate::linker::value_flow::TypeTarget<'a>>) -> crate::linker::value_flow::TypeTarget<'a> {
        crate::linker::value_flow::TypeTarget::RustTuple(items)
    }
    fn generic_sequence_return(&self) -> bool { false }
    fn collection_item<'a>(&self, target: &crate::linker::value_flow::TypeTarget<'a>) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::TypeTarget;
        match target {
            TypeTarget::RustIterator { item } | TypeTarget::RustCollection { item } | TypeTarget::RustSlice { item } | TypeTarget::RustTypedContainer { item, .. } => Some((**item).clone()),
            _ => None,
        }
    }
    fn project_applied_return<'a>(&self, name: &str, first: &crate::linker::value_flow::TypeTarget<'a>, second: &mut dyn FnMut() -> Option<crate::linker::value_flow::TypeTarget<'a>>) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::{RustWrapperKind, TypeTarget};
        Some(match name {
            "Option" => TypeTarget::RustWrapper { wrapper: RustWrapperKind::Option, ok: Box::new(first.clone()), err: None },
            "Result" => TypeTarget::RustWrapper { wrapper: RustWrapperKind::Result, ok: Box::new(first.clone()), err: second().map(Box::new) },
            "Iterator" => TypeTarget::RustIterator { item: Box::new(first.clone()) },
            "Vec" => TypeTarget::RustCollection { item: Box::new(first.clone()) },
            "Box" | "Arc" | "Rc" | "Cow" => TypeTarget::RustDeref { wrapper: match name { "Box" => "Box", "Arc" => "Arc", "Rc" => "Rc", _ => "Cow" }, target: Box::new(first.clone()) },
            "VecDeque" | "HashSet" | "BTreeSet" => TypeTarget::RustTypedContainer { base: match name { "VecDeque" => "VecDeque", "HashSet" => "HashSet", _ => "BTreeSet" }, item: Box::new(first.clone()) },
            "Slice" => TypeTarget::RustSlice { item: Box::new(first.clone()) },
            _ => return None,
        })
    }
    fn resolve_applied_type<'a>(&self, context: &crate::linker::semantic_context::context::Context<'a>, owner: &'a Node, (base, args): (&str, &[crate::semantic::TypeExpr]), nominal: &crate::linker::value_flow::Symbol<'a>, at: crate::semantic::SourcePosition) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::semantic_context::nominal_application(owner, nominal)
            .or_else(|| super::value_flow::resolve_applied(owner, base, args, nominal, at, context))
    }
    fn admits_type_payload<'a>(&self, owner: &Node, wrapper: &crate::linker::value_flow::TypeTarget<'a>, count: usize) -> bool {
        super::semantic_context::admits_payload(owner, wrapper, count)
    }
    fn wrapper_value_member(&self, wrapper: &str, member: &str) -> bool {
        super::semantic_context::wrapper_value_member(wrapper, member)
    }
    fn standard_external_member(&self, module: &str, member: &str) -> bool {
        super::semantic_context::standard_member(module, member)
    }
    fn structural_value<'a>(&self, target: &crate::linker::value_flow::TypeTarget<'a>, operation: crate::semantic::types::RustProjection, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::project_structural(target, operation, resolver)
    }
    fn adapter_value<'a>(&self, target: &crate::linker::value_flow::TypeTarget<'a>, member: &str, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::project(target, member, resolver)
    }
    fn declared_adapter_value<'a>(&self, target: &crate::linker::value_flow::TypeTarget<'a>, member: &str, owner: &'a Node, at: crate::semantic::SourcePosition, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::declared_adapter_result(target, member, owner, at, resolver)
    }
    fn deserialize_value<'a>(&self, owner: &'a Node, callee: &str, payload: &crate::semantic::TypeExpr, at: crate::semantic::SourcePosition, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>, limits: crate::linker::value_flow::SemanticLimits) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::deserializer_result(owner, callee, payload, at, resolver, limits)
    }
    fn is_value_adapter(&self, member: &str) -> bool {
        super::value_flow::is_adapter(member)
    }
    fn value_payload<'a>(&self, target: &crate::linker::value_flow::TypeTarget<'a>) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::unwrap_payload(target)
    }
    fn preserves_external_import_line(&self) -> bool { true }
    fn explicit_module_visible(&self, query: &crate::linker::traits::ExplicitModuleVisibility<'_, '_>) -> bool {
        query.input.semantic_context.import_state("rust").is_none_or(|state| state.module_path_visible(&query.node.qualname, query.file_module))
    }
    fn supports_static_import_receiver(&self) -> bool { true }
    fn allows_typed_member_read(&self, _known_receiver: bool) -> bool { true }
    fn supports_constant_receiver(&self) -> bool { true }
    fn associated_module_owner(&self, node: &Node) -> bool { node.kind == crate::models::tags::NodeKind::Module }
    fn admits_lexical_candidate(&self, node: &Node) -> bool { node.kind != crate::models::tags::NodeKind::Impl }
    fn imported_callable<'a>(&self, phase: &crate::linker::traits::ReferencePhase<'a, '_>, targets: &[&'a Node], member: &str) -> Vec<&'a Node> {
        phase.owner.filter(|owner| owner.language == crate::models::tags::LanguageId::Rust)
            .map_or_else(Vec::new, |owner| phase.input.semantic_context.resolve_rust_imported_call(owner, targets, member))
    }
    fn dynamic_reference(&self, phase: &mut crate::linker::traits::ReferencePhase<'_, '_>) -> bool {
        super::resolution::dynamic_receiver_member(phase.input, phase.path, phase.reference, phase.owner, phase.graph)
    }
    fn early_reference(&self, phase: &mut crate::linker::traits::ReferencePhase<'_, '_>) -> bool {
        let lookup = crate::linker::semantic_context::LanguageMemberLookup::new(phase.input.semantic_context);
        super::resolution::resolve_self_reference(phase.profile, phase.reference, phase.owner, phase.path, &lookup, phase.graph)
    }
    fn member_owner_scope<'a>(&self, scope: &'a str) -> &'a str {
        scope.split_once('<').map_or(scope, |(base, _)| base)
    }
    fn associated_member_owner(&self, node: &Node) -> bool {
        matches!(node.kind, crate::models::tags::NodeKind::Struct | crate::models::tags::NodeKind::Enum | crate::models::tags::NodeKind::Trait)
    }
    fn explicit_constant_member<'a>(&self, constant: &'a Node, member: &str, owner: &'a Node,
        resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>) -> Option<&'a Node> {
        use crate::linker::value_flow::{node_position, resolve_type, SemanticLimits, Symbol};
        let alias = resolver.value_flow(constant)?.alias_type.as_ref()?;
        let target = resolve_type(constant, alias, node_position(constant), resolver, SemanticLimits::default());
        match resolver.resolve_member(&target, member, owner, node_position(owner)) {
            Symbol::Callable(node) => Some(node),
            _ => None,
        }
    }
    fn explicit_reexport_targets<'a>(&self, query: crate::linker::traits::ExplicitReexport<'a, '_>) -> Vec<&'a Node> {
        super::resolution::explicit_reexport_targets(query.input, query.file_module, query.target, query.lookup_key)
    }
    fn implementation_source(&self, input: crate::linker::traits::ImplementationSource<'_, '_>) -> Option<(Option<String>, bool)> {
        Some(super::resolution::impl_source(input.implementation, &input.implementation.qualname,
            input.file_module, input.by_qual, input.by_name, input.aliases, input.lookup_key))
    }
    fn property_before_member(&self) -> bool { true }

    fn record_candidate_gaps(
        &self, profile: &dyn crate::languages::LanguageProfile, reference: &crate::models::Reference,
        candidates: &[&Node], path: &str, graph: &mut crate::models::compact_graph::CompactGraph,
    ) {
        crate::linker::rust::resolution::macro_analysis_gap(Some(profile), reference, candidates, path, graph);
    }

    fn macro_reference_candidate(&self, owner: Option<&Node>, candidate: &Node) -> bool {
        owner.is_some_and(|owner| owner.kind == crate::models::tags::NodeKind::Macro)
            && matches!(candidate.kind, crate::models::tags::NodeKind::Function | crate::models::tags::NodeKind::Method)
    }

    fn explicit_module_target(
        &self,
        profile: &dyn LanguageProfile,
        phase: &crate::linker::traits::ReferencePhase<'_, '_>,
    ) -> Option<crate::linker::traits::ExplicitModuleTarget> {
        super::resolution::rust_explicit_module_target(
            phase.input,
            Some(profile),
            phase.path,
            phase.file_module,
            &phase.reference.expression,
        )
    }

    fn special_reference(&self, phase: &mut crate::linker::traits::ReferencePhase<'_, '_>) -> bool {
        super::resolution::trait_object_call(
            phase.profile,
            phase.reference,
            phase.owner,
            phase.path,
            phase.input.value_flow,
            phase.input.semantic_context,
            phase.graph,
        )
    }

    fn explicit_module_prefix(&self, expression: &str) -> bool {
        ["super::", "crate::", "self::"]
            .iter()
            .any(|prefix| expression.starts_with(prefix))
    }

    fn builtin_evidence(&self, first: &str, expression: &str) -> Option<(&'static str, String)> {
        (first != "Self").then(|| {
            (
                "Rust standard library",
                format!("Rust standard library prelude: {expression}"),
            )
        })
    }

    fn external_annotation(
        &self,
        profile: &dyn LanguageProfile,
        head: &str,
        path: &str,
        dependencies: &DependencyRegistry,
    ) -> bool {
        super::semantic_context::external_annotation_type(
            Some(profile),
            head,
            head,
            path,
            dependencies,
        )
        .is_some()
    }

    fn callable_constant(&self, node: &Node) -> bool {
        node.kind == crate::models::tags::NodeKind::Constant
    }

    fn builtin_origin(&self) -> Option<&'static str> {
        Some("Rust standard library")
    }

    fn returns_parent_enum(&self, node: &Node) -> bool {
        node.kind == crate::models::tags::NodeKind::Constant && node.detail_entry(crate::models::details::NodeDetailKey::RUST_ENUM_VARIANT) == true
    }

    fn uses_shared_class_members(&self) -> bool {
        false
    }

    fn uses_hierarchy_rules(&self) -> bool {
        true
    }

    fn callable_marker_evidence(
        &self,
        receiver: &crate::linker::value_flow::TypeTarget<'_>,
    ) -> Option<&'static str> {
        matches!(receiver, crate::linker::value_flow::TypeTarget::RustClosure)
            .then_some("invocation of a lexically proven Rust closure")
    }


    fn filter_call_candidates(
        &self,
        member_state: Option<&dyn crate::languages::profile::LanguageMemberIndex>,
        edge_kind: &str,
        candidates: &mut Vec<&Node>,
    ) {
        let Some(state) = member_state else {
            return;
        };
        if candidates.len() > 1
            && (edge_kind == EdgeKind::Calls.as_str() || edge_kind == EdgeKind::Handles.as_str())
            && candidates.iter().any(|node| state.is_inherent_member(node))
            && candidates
                .iter()
                .any(|node| !state.is_inherent_member(node))
        {
            candidates.retain(|node| state.is_inherent_member(node));
        }
    }

    fn admits_dynamic_member(
        &self,
        member_state: Option<&dyn crate::languages::profile::LanguageMemberIndex>,
        dynamic: bool,
        node: &Node,
    ) -> bool {
        !dynamic || member_state.is_some_and(|state| state.is_inherent_member(node))
    }

    fn value_projection(
        &self,
        member: &str,
    ) -> Option<crate::languages::profile::LanguageValueProjection> {
        use crate::languages::profile::LanguageValueProjection as Projection;
        Some(match member {
            adapter if super::value_flow::is_adapter(adapter) => Projection::Adapter,
            _ => return None,
        })
    }

    fn evaluate_call_fallback<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        callee: &str,
        _site: Option<&crate::semantic::CallSiteEvidence>,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::constructor_result(
            eval.index,
            eval.owner,
            callee,
            eval.at,
            eval.resolver,
            eval.trace,
        )
    }

    fn local_receiver_class<'a>(
        &self,
        target: &crate::linker::value_flow::TypeTarget<'a>,
    ) -> Option<&'a Node> {
        super::value_flow::local_class(target)
    }

    fn unwrap_reference<'t, 'a>(
        &self,
        target: &'t crate::linker::value_flow::TypeTarget<'a>,
    ) -> &'t crate::linker::value_flow::TypeTarget<'a> {
        super::value_flow::referent(target).0
    }

    fn normalize_file_imports<'a>(
        &self,
        profile: &dyn LanguageProfile,
        path: &'a str,
        facts: &'a Facts,
        ctx: &ImportNormalizationContext<'_, 'a>,
        out: &mut HashMap<(&'a str, &'a str), Option<ImportPath>>,
    ) {
        let root_rust_crate = ctx.dependencies.root_rust_crate_for_path(path).filter(|_| {
            let workspace = languages::workspace_path(path).0;
            if workspace.is_empty() {
                ctx.profiles.contains_key("src/lib.rs")
            } else {
                ctx.profiles
                    .contains_key(format!("{workspace}/src/lib.rs").as_str())
            }
        });
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            let module = reference.module.as_deref().unwrap_or(&reference.expression);
            out.entry((path, module)).or_insert_with(|| {
                normalize_single_rust_import(profile, path, module, facts, ctx, root_rust_crate)
            });
        }
    }

    fn module_namespaces(
        &self,
        _profile: &dyn LanguageProfile,
        node: &Node,
        dependency_registry: &DependencyRegistry,
        out: &mut Vec<String>,
    ) {
        let (workspace, local_path) = languages::workspace_path(&node.path);
        if local_path == "src/lib.rs" {
            let is_workspace_lib = dependency_registry
                .rust_library_for_path(&node.path)
                .is_some();
            if is_workspace_lib || !out.iter().any(|existing| existing == "src") {
                out.push("src".to_owned());
            }
            let _ = workspace;
        }
        if let Some(prefix) = node.path.strip_suffix("/src/lib.rs") {
            out.push(format!("{}.src", prefix.replace('/', ".")));
        }
    }

    fn needs_reference_identity(&self, _path: &str, facts: &Facts) -> bool {
        facts
            .references
            .iter()
            .any(|reference| reference.kind == EdgeKind::Imports.as_str())
    }

    fn reference_identity_changed(&self, _path: &str, old: &Facts, new: &Facts) -> bool {
        fn imports(facts: &Facts) -> HashSet<(&str, Option<&str>, Option<&str>)> {
            facts
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
                .map(|reference| {
                    (
                        reference.expression.as_str(),
                        reference.alias.as_deref(),
                        reference.module.as_deref(),
                    )
                })
                .collect()
        }

        imports(old) != imports(new)
    }

    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        let requester = context
            .by_module
            .get(context.path)
            .into_iter()
            .flatten()
            .find(|node| node.id == context.reference.source)
            .or_else(|| {
                context
                    .by_module
                    .get(context.path)
                    .into_iter()
                    .flatten()
                    .find(|node| node.kind == crate::models::tags::NodeKind::Module)
            })
            .map_or("", |node| node.qualname.as_str());
        if context.language_state.is_some_and(|state| {
            context.normalized.is_some_and(|normalized| {
                !state.module_path_visible(&normalized.namespace, requester)
            })
        }) {
            context.modules.clear();
            context.candidates.clear();
            return ImportResolution::default();
        }
        let mut resolution = resolve_default_import(context);
        context.candidates.retain(|node| {
            node.kind != crate::models::tags::NodeKind::Impl
                && (node.kind == crate::models::tags::NodeKind::Module
                    && context
                        .language_state
                        .is_none_or(|state| state.module_path_visible(&node.qualname, requester))
                    || is_crate_visible(node, requester)
                    || node
                        .qualname
                        .rsplit_once('.')
                        .is_some_and(|(scope, _)| within(requester, module_namespace(scope))))
        });
        if !context.candidates.is_empty() {
            return resolution;
        }
        if let Some(normalized) = context.normalized {
            if let Some(nodes) = context.by_qual.get(normalized.namespace.as_str()) {
                context
                    .candidates
                    .extend(nodes.iter().copied().filter(|n| n.path == context.path));
                if !context.candidates.is_empty() {
                    return resolution;
                }
            }
            if let Some(owner) = context
                .by_module
                .get(context.path)
                .into_iter()
                .flatten()
                .find(|n| n.id == context.reference.source)
            {
                let owner_qual = format!(
                    "{}.{}",
                    owner.qualname,
                    context.reference.expression.replace("::", ".")
                );
                if let Some(nodes) = context.by_qual.get(owner_qual.as_str()) {
                    context
                        .candidates
                        .extend(nodes.iter().copied().filter(|n| n.path == context.path));
                    if !context.candidates.is_empty() {
                        if context.modules.is_empty() {
                            context.modules.push(owner);
                        }
                        return resolution;
                    }
                }
            }
        }
        let Some(forwarding) = context.language_state else {
            return resolution;
        };
        let requester_node = context
            .by_module
            .get(context.path)
            .into_iter()
            .flatten()
            .find(|node| node.kind == crate::models::tags::NodeKind::Module);
        if context.modules.is_empty() && context.normalized.is_none() {
            let raw = context
                .reference
                .module
                .as_deref()
                .unwrap_or(&context.reference.expression);
            if let Some(req_mod) = requester_node.filter(|_| raw.contains("::")) {
                let resolved = forwarding.resolve_path(
                    req_mod,
                    &raw.replace("::", "."),
                    requester,
                    Some(context.by_qual),
                );
                if !resolved.is_empty() {
                    context.modules.push(req_mod);
                    context.candidates.extend(resolved);
                    resolution.reexport_symbol = true;
                }
            }
            return resolution;
        }
        let Some(name) = context
            .tail()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
        else {
            return resolution;
        };
        if context.modules.len() != 1 {
            return resolution;
        }
        let module = context.modules[0];
        if module.language != crate::models::tags::LanguageId::Rust {
            return resolution;
        }
        let requester =
            requester_node.map_or(module.qualname.as_str(), |owner| owner.qualname.as_str());
        context.candidates.extend(forwarding.resolve_path(
            module,
            &name,
            requester,
            Some(context.by_qual),
        ));
        if !context.candidates.is_empty()
            && context
                .candidates
                .iter()
                .all(|node| node.kind != crate::models::tags::NodeKind::Module || node.path != module.path)
        {
            resolution.reexport_symbol = true;
        }
        resolution
    }

    fn resolve_imported_member<'a>(
        &self,
        interner: &LinkerInterner<'a>,
        module: &'a Node,
        member: &str,
        by_module: &HashMap<&'a str, Vec<&'a Node>>,
        by_qual: &HashMap<&'a str, Vec<&'a Node>>,
        exports: &PackageExports<'a>,
        lookup_key: &mut String,
    ) -> Vec<&'a Node> {
        if let Some(Some(target)) = package_export(exports, interner, module.path.as_str(), member)
        {
            return vec![target];
        }
        lookup_key.clear();
        lookup_key.push_str(&module.qualname);
        lookup_key.push('.');
        lookup_key.push_str(member);
        let direct: Vec<&'a Node> = by_module
            .get(module.path.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| node.qualname == lookup_key.as_str() && node.kind != crate::models::tags::NodeKind::Module)
            .collect();
        if !direct.is_empty() {
            return direct;
        }
        if member.contains('.') {
            let is_visible_imported = |node: &Node| {
                node.language == crate::models::tags::LanguageId::Rust
                    && !matches!(node.kind.as_str(), "module" | "impl")
                    && (node.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) == true
                        || (crate_namespace(&module.path) == crate_namespace(&node.path)
                            && node.detail_entry(crate::models::details::NodeDetailKey::SIGNATURE)
                                .as_str()
                                .is_some_and(|sig| sig.starts_with("pub("))))
            };
            let mut targets: Vec<&'a Node> = by_qual
                .get(lookup_key.as_str())
                .into_iter()
                .flatten()
                .copied()
                .filter(|node| is_visible_imported(node))
                .collect();
            if targets.is_empty() {
                let ns = module_namespace(&module.qualname);
                if ns != module.qualname {
                    lookup_key.clear();
                    lookup_key.push_str(ns);
                    lookup_key.push('.');
                    lookup_key.push_str(member);
                    targets.extend(
                        by_qual
                            .get(lookup_key.as_str())
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|node| is_visible_imported(node)),
                    );
                }
            }
            if !targets.is_empty() {
                return targets;
            }
        }
        let prefix = format!("{}.", module.qualname);
        let items: Vec<&'a Node> = by_module
            .get(module.path.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| {
                node.qualname.starts_with(&prefix)
                    && matches!(node.kind.as_str(), "macro" | "function")
            })
            .collect();
        if let [single] = items.as_slice() {
            return vec![single];
        }
        items
            .into_iter()
            .filter(|node| node.name == member)
            .collect()
    }

    fn package_exports<'a>(
        &self,
        interner: &mut LinkerInterner<'a>,
        _node_rebindings: &NodeBindingIndex,
        all: &'a dyn crate::models::FactsLookup,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&std::path::Path>,
    ) -> PackageExports<'a> {
        declared_exports(all.values_facts(), interner)
    }

    fn required_full_facts(
        &self,
        facts: &dyn crate::models::FactsLookup,
        affected: &std::collections::BTreeSet<String>,
        catalog: &[(&str, &str)],
    ) -> std::collections::BTreeSet<String> {
        super::profile_linker::required_full_facts(facts, affected, catalog)
    }

    fn apply_wildcard_import<'a, 'file>(&self, state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>, reference: &'a crate::models::Reference, profile: Option<&dyn LanguageProfile>, scope: &'file str, modules: &[&'a Node]) -> bool where 'a: 'file {
        #[cfg(feature = "lang-rust")]
        if let [target_module] = modules {
            if state.file_module.strip_prefix(&target_module.qualname).is_some_and(|s| s.starts_with('.')) {
                apply_rust_parent_wildcard(state, reference, profile, scope, target_module);
            }
            if state.input.interner.lookup_node(state.input.by_id, reference.source.as_str()).is_some_and(|owner| owner.kind != crate::models::tags::NodeKind::Module) {
                apply_rust_scope_wildcard_bindings(state, reference, scope, target_module);
            }
        }
        #[cfg(not(feature = "lang-rust"))]
        let _ = (state, reference, profile, scope, modules);
        true
    }

    fn apply_aliased_external_fallback<'a, 'file>(&self, state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>, reference: &'a crate::models::Reference, normalized: Option<&ImportPath>, scope: &'file str, alias: &'a str) where 'a: 'file {
        #[cfg(feature = "lang-rust")]
        if normalized.is_some_and(|import| import.relative) {
            if let Some(crate::linker::semantic_context::Imported::External(origin, position, _, _)) = state.input.interner.lookup_symbol(&reference.source)
                .and_then(|owner| state.input.semantic_context.imports.get(&owner))
                .and_then(|bindings| state.input.interner.lookup_symbol(alias).and_then(|name| bindings.get(&name)))
            {
                state.external_aliases.entry(scope).or_default().insert(alias, Some((origin.replace("::", "."), position.line)));
            }
        }
        #[cfg(not(feature = "lang-rust"))]
        let _ = (state, reference, normalized, scope, alias);
    }

    fn apply_module_star_reexports<'a, 'file>(&self, state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>) where 'a: 'file {
        #[cfg(feature = "lang-rust")]
        apply_rust_module_star_reexports(state);
        #[cfg(not(feature = "lang-rust"))]
        let _ = state;
    }

    fn checks_annotation_binding_shadowing(&self) -> bool { false }
    fn module_shadows_annotation(&self) -> bool { true }
    fn admits_external_generic_annotation(&self, profile: Option<&dyn LanguageProfile>, raw_annotation: &str, head: &str, module: &str) -> bool {
        !(rust_has_non_lifetime_generics(raw_annotation) && !matches!(module.split("::").next(), Some("std" | "core" | "alloc")) && !profile.is_some_and(|p| p.builtin_generic(head)))
    }
    fn external_parameter_annotation<'a>(&self, profile: Option<&dyn LanguageProfile>, raw_annotation: &'a str, raw_head: &'a str, head: &str, _member: &str, path: &str, dependencies: &DependencyRegistry, try_qualified: bool) -> Option<crate::linker::types::ReceiverType<'a>> {
        use crate::linker::types::ReceiverType;
        match head {
            "std" => return Some(ReceiverType::External("std", 0)),
            "core" => return Some(ReceiverType::External("core", 0)),
            "alloc" => return Some(ReceiverType::External("alloc", 0)),
            _ => {}
        }
        #[cfg(feature = "lang-rust")]
        {
            if try_qualified {
                if let Some(q) = super::semantic_context::qualified_external_annotation(profile, raw_annotation, head, path, dependencies) {
                    return Some(q);
                }
            }
            super::semantic_context::external_annotation_type(profile, raw_head, head, path, dependencies)
        }
        #[cfg(not(feature = "lang-rust"))]
        { let _ = (profile, raw_annotation, raw_head, path, dependencies, try_qualified); None }
    }
    fn symbol_visible(&self, owner: &Node, candidate: &Node) -> Option<bool> { super::value_flow::visible(owner, candidate) }
    fn prepare_semantic_context<'a>(&self, context: &mut crate::linker::semantic_context::context::Context<'a>, prep: &crate::linker::traits::SemanticContextPrep<'a>) {
        #[cfg(feature = "lang-rust")]
        super::semantic_context::import_globs(context, prep.all, prep.by_id, prep.normalized_imports, prep.modules);
        #[cfg(not(feature = "lang-rust"))]
        let _ = (context, prep);
    }
    fn resolve_special_type_expr<'a>(&self, owner: &'a Node, expression: &crate::semantic::TypeExpr, at: crate::semantic::SourcePosition, resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>, limits: crate::linker::value_flow::SemanticLimits) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        use crate::linker::value_flow::{resolve_type, TypeTarget};
        use crate::semantic::TypeExpr;
        match expression {
            TypeExpr::RustRange { item } => Some(TypeTarget::RustDeref { wrapper: "Range", target: Box::new(TypeTarget::RustIterator { item: Box::new(resolve_type(owner, item, at, resolver, limits)) }) }),
            TypeExpr::RustArray { item } => Some(TypeTarget::RustSlice { item: Box::new(resolve_type(owner, item, at, resolver, limits)) }),
            TypeExpr::Applied { base, args } if base == "()" => Some(TypeTarget::RustTuple(args.iter().map(|i| resolve_type(owner, i, at, resolver, limits)).collect())),
            TypeExpr::Applied { base, args } if matches!(base.as_str(), "&" | "&mut") => Some(match args.as_slice() {
                [target] => match resolve_type(owner, target, at, resolver, limits) {
                    inner @ TypeTarget::External { .. } => inner,
                    inner => TypeTarget::RustReference { mutable: base == "&mut", target: Box::new(inner) },
                },
                _ => TypeTarget::Unknown,
            }),
            _ => None,
        }
    }
    fn stored_type_expr(&self, annotation: &str) -> Option<crate::semantic::TypeExpr> {
        let mut rest = annotation.strip_prefix('&')?.trim_start();
        if rest.starts_with('\'') {
            rest = rest.split_once(char::is_whitespace)?.1.trim_start();
        }
        let mutable = rest.starts_with("mut ");
        let rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
        Some(crate::semantic::TypeExpr::Applied {
            base: if mutable { "&mut" } else { "&" }.to_owned(),
            args: vec![crate::linker::value_flow::stored_type_expr(rest, "rust")],
        })
    }
    fn disambiguate_candidates(&self, candidates: &mut Vec<&Node>) { disambiguate_rust_cfg_siblings(candidates); }
}

fn rust_has_non_lifetime_generics(annotation: &str) -> bool {
    let Some(name) = crate::linker::types::strip_rust_references(annotation, true) else {
        return false;
    };
    let Some((_, rest)) = name.split_once('<') else {
        return false;
    };
    let Some(inner) = rest.strip_suffix('>') else {
        return false;
    };
    inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| !part.starts_with('\''))
}

#[cfg(feature = "lang-rust")]
fn apply_rust_parent_wildcard<'a, 'file>(
    state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>,
    r: &crate::models::Reference,
    profile: Option<&dyn LanguageProfile>,
    scope: &'file str,
    target_module: &'a Node,
) where
    'a: 'file,
{
    let input = state.input;
    let path = state.path;
    let file_module = state.file_module;
    let Some(target_facts) = input.all.get_facts(&target_module.path) else {
        return;
    };
    let workspace = languages::workspace_path(path).0;
    for parent_import in target_facts.references.iter().filter(|import| {
        import.kind == "imports"
            && import.source == target_module.id
            && import.alias.as_deref().is_some_and(|alias| alias != "*")
    }) {
        let module = parent_import
            .module
            .as_deref()
            .unwrap_or(&parent_import.expression);
        let Some(provider) = input
            .normalized_imports
            .get(&(target_module.path.as_str(), module))
            .and_then(Option::as_ref)
        else {
            continue;
        };
        let alias = parent_import.alias.as_deref().unwrap_or_default();
        if input
            .interner
            .lookup_node(input.by_id, r.source.as_str())
            .is_some_and(|owner| owner.kind != crate::models::tags::NodeKind::Module)
            && (state.references.iter().any(|reference| {
                reference.kind == "imports"
                    && reference.source == r.source
                    && reference.alias.as_deref() == Some(alias)
            }) || input
                .by_qual
                .get(crate::linker::types::qualified(
                    state.lookup_key,
                    scope,
                    alias,
                ))
                .is_some_and(|nodes| nodes.iter().any(|node| node.path == path)))
        {
            continue;
        }
        if target_facts
            .references
            .iter()
            .filter(|import| {
                import.kind == "imports"
                    && import.source == target_module.id
                    && import.alias.as_deref() == Some(alias)
            })
            .count()
            != 1
        {
            continue;
        }
        let mut local_providers: Vec<&Node> = input
            .by_qual
            .get(provider.namespace.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| {
                node.language == LanguageId::Rust
                    && languages::workspace_path(&node.path).0 == workspace
            })
            .collect();
        if local_providers.is_empty() {
            if let Some(import_state) = input.semantic_context.import_state("rust") {
                local_providers.extend(
                    import_state
                        .targets(target_module, alias, file_module)
                        .iter()
                        .copied(),
                );
            }
        }
        if !local_providers.is_empty() {
            state
                .aliases
                .entry(scope)
                .or_default()
                .entry(alias)
                .or_default()
                .extend(local_providers);
        } else if !provider.relative
            && !state
                .aliases
                .get(scope)
                .is_some_and(|entries| entries.contains_key(alias))
            && profile
                .and_then(|p| {
                    input.dependency_registry.classification_for_path(
                        p,
                        module,
                        &target_module.path,
                    )
                })
                .is_some()
        {
            state.aliases.entry(scope).or_default().entry(alias).or_default();
            state
                .external_aliases
                .entry(scope)
                .or_default()
                .insert(alias, Some((provider.namespace.clone(), r.line)));
        }
    }
}

#[cfg(feature = "lang-rust")]
fn apply_rust_scope_wildcard_bindings<'a, 'file>(
    state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>,
    r: &crate::models::Reference,
    scope: &'file str,
    target_module: &'a Node,
) where
    'a: 'file,
{
    let input = state.input;
    let Some(import_state) = input.semantic_context.import_state("rust") else {
        return;
    };
    for (name, targets) in import_state.visible_bindings(target_module, state.file_module) {
        if state.references.iter().any(|reference| {
            reference.kind == "imports"
                && reference.source == r.source
                && reference.alias.as_deref() == Some(name)
        }) {
            continue;
        }
        if input
            .by_qual
            .get(crate::linker::types::qualified(state.lookup_key, scope, name))
            .is_some_and(|nodes| nodes.iter().any(|node| node.path == state.path))
        {
            state.aliases.entry(scope).or_default().remove(name);
            continue;
        }
        match state.aliases.entry(scope).or_default().entry(name) {
            hashbrown::hash_map::Entry::Vacant(entry) => {
                entry.insert(targets.to_vec());
            }
            hashbrown::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().is_empty() || targets.is_empty() {
                    entry.get_mut().clear();
                } else {
                    entry.get_mut().extend_from_slice(targets);
                    entry.get_mut().sort_by_key(|node| node.id.as_str());
                    entry.get_mut().dedup_by_key(|node| node.id.as_str());
                }
                if !targets.is_empty() {
                    if let Some(origin) = state
                        .external_aliases
                        .get_mut(scope)
                        .and_then(|bindings| bindings.get_mut(name))
                    {
                        *origin = None;
                    }
                }
            }
        }
    }
}

#[cfg(feature = "lang-rust")]
fn apply_rust_module_star_reexports<'a, 'file>(
    state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>,
) where
    'a: 'file,
{
    let input = state.input;
    let Some(module) = state.facts.nodes.iter().find(|node| {
        node.kind == crate::models::tags::NodeKind::Module && node.language == LanguageId::Rust
    }) else {
        return;
    };
    if state.references.iter().any(|reference| {
        reference.kind == "imports"
            && reference.source == module.id
            && reference.alias.as_deref() == Some("*")
    }) {
        if let Some(import_state) = input.semantic_context.import_state("rust") {
            let bindings = state.aliases.entry(module.qualname.as_str()).or_default();
            for (name, targets) in import_state.visible_bindings(module, &module.qualname) {
                if input
                    .by_qual
                    .get(crate::linker::types::qualified(
                        state.lookup_key,
                        &module.qualname,
                        name,
                    ))
                    .is_some_and(|nodes| {
                        nodes.iter().any(|node| {
                            node.path == state.path
                                && node.kind != crate::models::tags::NodeKind::ModuleDeclaration
                        })
                    })
                {
                    bindings.remove(name);
                } else {
                    bindings.insert(name, targets.to_vec());
                }
            }
        }
    }
    let bindings = state.aliases.entry(module.qualname.as_str()).or_default();
    if let Some(imports) = input
        .interner
        .lookup_symbol(&module.id)
        .and_then(|id| input.semantic_context.imports.get(&id))
    {
        for imported in imports.values() {
            if let crate::linker::semantic_context::Imported::External(
                origin,
                position,
                reference,
                _,
            ) = imported
            {
                if let Some(name) = reference.alias.as_deref() {
                    if bindings.get(name).is_none_or(Vec::is_empty) {
                        state
                            .external_aliases
                            .entry(module.qualname.as_str())
                            .or_default()
                            .entry(name)
                            .or_insert_with(|| Some((origin.replace("::", "."), position.line)));
                    }
                }
            }
        }
    }
}

/// Disambiguate Rust candidate sets whose definitions are pairwise mutually exclusive
/// under `#[cfg(...)]` attributes. Non-exclusive siblings remain ambiguous (fail-closed).
pub(super) fn disambiguate_rust_cfg_siblings(candidates: &mut Vec<&Node>) {
    if candidates.len() <= 1
        || !candidates
            .iter()
            .all(|node| node.language == LanguageId::Rust)
    {
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
    let decorators = node
        .detail_entry(crate::models::details::NodeDetailKey::DECORATORS)
        .as_array()?;
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
