use super::super::semantic_context::{Context, Imported};
use super::super::types::NodeBindingIndex;
use super::super::value_flow::resolve_type;
use super::super::value_flow::types::{
    collect_candidate_join, SemanticLimits, SemanticResolver, Symbol, SymbolRole, TypeTarget,
};
use crate::models::{Facts, Node, Reference};
use crate::semantic::{SourcePosition, TypeExpr};
use crate::languages::{
    dependency_registry::DependencyRegistry, LanguageFamily, LanguageProfile,
};
use crate::interner::LinkerInterner;
use crate::linker::traits::ModulesByNamespace;

pub(in crate::linker) fn canonical_pathlib_class(class: &str) -> Option<&'static str> {
    match class {
        "Path" => Some("pathlib.Path"),
        "PosixPath" => Some("pathlib.PosixPath"),
        "WindowsPath" => Some("pathlib.WindowsPath"),
        "PurePath" => Some("pathlib.PurePath"),
        "PurePosixPath" => Some("pathlib.PurePosixPath"),
        "PureWindowsPath" => Some("pathlib.PureWindowsPath"),
        _ => None,
    }
}

pub(in crate::linker) fn receiver_access(owner: &Node, head: &str) -> Option<(bool, bool)> {
    (owner.language == crate::models::tags::LanguageId::Python
        && matches!(head, "self" | "cls")
        && owner.detail_entry(crate::models::details::NodeDetailKey::RECEIVER_NAME).as_str() == Some(head))
    .then_some((false, false))
}

pub(in crate::linker) fn is_external_generic_base(
    module: &str,
    clean_base: &str,
    profile: Option<&dyn LanguageProfile>,
) -> bool {
    matches!(module, "typing" | "typing_extensions" | "collections.abc")
        && profile.is_some_and(|profile| profile.builtin_generic(clean_base))
}

fn external_stdlib_factory<'a>(
    context: &Context<'a>,
    owner: &'a Node,
    callee: &str,
    at: SourcePosition,
    (imported_module, import_line, reference_expr, member): (&str, usize, &str, &str),
) -> Option<TypeTarget<'a>> {
    let canonical = match (imported_module, reference_expr, member) {
        ("logging", "logging", "getLogger") | ("logging", "getLogger", "") => "logging.Logger",
        ("logging", "logging", "LoggerAdapter") | ("logging", "LoggerAdapter", "") => {
            "logging.LoggerAdapter"
        }
        ("sqlite3", "sqlite3", "connect") | ("sqlite3", "connect", "") => "sqlite3.Connection",
        ("pathlib", "pathlib", cls) | ("pathlib", cls, "") => canonical_pathlib_class(cls)?,
        _ => return None,
    };
    let Symbol::Type(TypeTarget::External {
        module,
        import_line: resolved_line,
    }) = context.resolve_symbol(owner, callee, at, SymbolRole::Callable)
    else {
        return None;
    };
    if (!matches!(module.as_str(), "logging" | "sqlite3" | "pathlib")
        && !module.starts_with("pathlib."))
        || import_line != resolved_line
    {
        return None;
    }
    Some(TypeTarget::Builtin(canonical.to_owned()))
}

fn external_configured_factory<'a>(
    context: &Context<'a>,
    owner: &'a Node,
    callee: &str,
    imported_module: &str,
    import_line: usize,
    exported: &str,
) -> Option<TypeTarget<'a>> {
    let mut receivers = context.receiver_types_for_path(&owner.path)?;
    let receiver = receivers.find(|receiver| {
        receiver.module.as_deref() == Some(imported_module)
            && receiver.export.as_deref() == Some(exported)
            && receiver
                .callback_callees
                .iter()
                .any(|allowed| allowed == callee || allowed.rsplit('.').next() == Some(exported))
    })?;
    let (pack, authority) = context.hierarchy_authority_for_base(&owner.path, &receiver.name)?;
    Some(TypeTarget::ConfiguredReceiver {
        authority,
        origin: Box::new(TypeTarget::External {
            module: imported_module.to_owned(),
            import_line,
        }),
        fallback: None,
        evidence: format!(
            "{}:{}:{}:{}",
            context.dependencies_digest(),
            owner.path,
            pack,
            receiver.name
        ),
    })
}

pub(in crate::linker) fn external_factory_result<'a>(
    context: &Context<'a>,
    owner: &'a Node,
    callee: &str,
    at: SourcePosition,
) -> Option<TypeTarget<'a>> {
    if owner.language != crate::models::tags::LanguageId::Python {
        return None;
    }
    let (head, member) = callee.split_once('.').unwrap_or((callee, ""));
    let mut scope = owner.qualname.as_str();
    loop {
        for node in context.nodes_in_path(scope, &owner.path) {
            if let Some(import) = context.imported_alias(node, head) {
                let Imported::External(imported_module, position, reference, safe) = import else {
                    return None;
                };
                if *position > at {
                    return None;
                }
                let stdlib_safe = imported_module == "pathlib"
                    || (*safe && matches!(imported_module.as_str(), "logging" | "sqlite3"));
                if stdlib_safe {
                    return external_stdlib_factory(
                        context,
                        owner,
                        callee,
                        at,
                        (imported_module, position.line, &reference.expression, member),
                    );
                }
                let exported = if reference.expression == *imported_module {
                    member
                } else if member.is_empty() {
                    reference.expression.as_str()
                } else {
                    return None;
                };
                return external_configured_factory(
                    context,
                    owner,
                    callee,
                    imported_module,
                    position.line,
                    exported,
                );
            }
        }
        scope = scope.rsplit_once('.')?.0;
    }
}

pub(in crate::linker) enum ExternalMemberOutcome {
    Supported(&'static str),
    Unsupported,
}

pub(in crate::linker) fn external_member(
    owner: &Node,
    module: &str,
    member: &str,
) -> Option<ExternalMemberOutcome> {
    if owner.language != crate::models::tags::LanguageId::Python {
        return None;
    }

    let origin = match module {
        "sqlite3.Cursor" | "sqlite3.Connection" => "builtin:sqlite3",
        "logging.Logger" => "builtin:logging.Logger",
        "logging.LoggerAdapter" => "builtin:logging.LoggerAdapter",
        _ if module.starts_with("pathlib.") => "builtin:pathlib",
        _ => return None,
    };

    Some(
        if crate::languages::by_id("python")
            .is_some_and(|profile| profile.builtin_member(module, member))
        {
            ExternalMemberOutcome::Supported(origin)
        } else {
            ExternalMemberOutcome::Unsupported
        },
    )
}

pub(in crate::linker) fn is_nullable_generic(
    owner: &Node,
    clean_base: &str,
    nominal: &Symbol<'_>,
) -> bool {
    owner.language == crate::models::tags::LanguageId::Python
        && match nominal {
            Symbol::Type(TypeTarget::Builtin(wrapper)) => {
                matches!(wrapper.as_str(), "Optional" | "Union")
            }
            Symbol::Unknown => matches!(clean_base, "Union" | "Optional"),
            _ => false,
        }
}

pub(in crate::linker) fn typing_nullable_generic(
    module: &str,
    reference: &Reference,
    member: &str,
) -> Option<&'static str> {
    if !matches!(module, "typing" | "typing_extensions") {
        return None;
    }
    match (reference.expression.as_str(), member) {
        ("Optional", "") => Some("Optional"),
        ("Union", "") => Some("Union"),
        (origin, "Optional") if origin == module => Some("Optional"),
        (origin, "Union") if origin == module => Some("Union"),
        _ => None,
    }
}

pub(in crate::linker) fn external_import_type<'a>(
    owner: &Node,
    module: &str,
    reference: &Reference,
    head: &str,
    member: &str,
    role: SymbolRole,
    import_line: usize,
) -> Option<Symbol<'a>> {
    if owner.language != crate::models::tags::LanguageId::Python {
        return None;
    }
    if matches!(role, SymbolRole::Constructor) {
        return Some(Symbol::Unknown);
    }

    let nullable = typing_nullable_generic(module, reference, member);
    let typing_callable = (reference.expression == "Callable" && member.is_empty())
        || (reference.expression == module && member == "Callable");
    if matches!(module, "typing" | "typing_extensions" | "collections.abc")
        && (nullable.is_some()
            || typing_callable
            || crate::languages::by_id("python").is_some_and(|profile| {
                profile.builtin_generic(head) || profile.builtin_member(head, "get")
            }))
    {
        return Some(Symbol::Type(TypeTarget::Builtin(
            nullable
                .unwrap_or(if typing_callable { "Callable" } else { head })
                .to_owned(),
        )));
    }

    let external_module = match module {
        "logging" => {
            if matches!(member, "Logger" | "LoggerAdapter") {
                Some(format!("logging.{member}"))
            } else if matches!(reference.expression.as_str(), "Logger" | "LoggerAdapter")
                && member.is_empty()
            {
                Some(format!("logging.{}", reference.expression))
            } else if matches!(head, "Logger" | "LoggerAdapter") {
                Some(format!("logging.{head}"))
            } else {
                None
            }
        }
        "pathlib" => {
            let class = if reference.expression == "pathlib" {
                member
            } else if member.is_empty() {
                reference.expression.as_str()
            } else {
                ""
            };
            canonical_pathlib_class(class).map(str::to_owned)
        }
        "sqlite3"
            if matches!(role, SymbolRole::Type)
                && ((reference.expression == "sqlite3"
                    && matches!(member, "Cursor" | "Connection"))
                    || (matches!(reference.expression.as_str(), "Cursor" | "Connection")
                        && member.is_empty())) =>
        {
            let class = if reference.expression == "sqlite3" {
                member
            } else {
                reference.expression.as_str()
            };
            Some(format!("sqlite3.{class}"))
        }
        _ => None,
    };
    if let Some(module) = external_module {
        return Some(Symbol::Type(TypeTarget::External {
            module,
            import_line,
        }));
    }

    None
}

pub(in crate::linker) fn collect_union_members<'a>(
    expr: &'a TypeExpr,
    out: &mut Vec<&'a TypeExpr>,
    depth: usize,
) {
    if depth >= crate::languages::MAX_VALUE_FLOW_TYPE_DEPTH {
        return;
    }
    match expr {
        TypeExpr::Named { name } if name == "None" => {}
        TypeExpr::Applied { base, args } => {
            let clean = base.rsplit_once('.').map_or(base.as_str(), |(_, n)| n);
            if matches!(clean, "Union" | "Optional") {
                for arg in args {
                    collect_union_members(arg, out, depth + 1);
                }
            } else {
                out.push(expr);
            }
        }
        _ => out.push(expr),
    }
}

pub(in crate::linker) fn resolve_applied<'a>(
    context: &Context<'a>,
    owner: &'a Node,
    base: &str,
    args: &[TypeExpr],
    nominal: &Symbol<'a>,
    at: SourcePosition,
) -> Option<TypeTarget<'a>> {
    if owner.language != crate::models::tags::LanguageId::Python {
        return None;
    }
    let clean_base = base.rsplit_once('.').map_or(base, |(_, name)| name);
    if is_nullable_generic(owner, clean_base, nominal) {
        if owner.kind == crate::models::tags::NodeKind::Type {
            return Some(TypeTarget::Unknown);
        }
        if clean_base == "Optional" && args.len() != 1 {
            return Some(TypeTarget::Unknown);
        }
        let mut members = Vec::new();
        for arg in args {
            collect_union_members(arg, &mut members, 0);
        }
        return Some(match members.as_slice() {
            [] => TypeTarget::Unknown,
            [single] => resolve_type(owner, single, at, context, SemanticLimits::default()),
            many => match collect_candidate_join(
                many.iter()
                    .map(|member| resolve_type(owner, member, at, context, SemanticLimits::default())),
            ) {
                TypeTarget::Candidates {
                    exhausted: true, ..
                } => TypeTarget::Candidates {
                    targets: Vec::new(),
                    complete: false,
                    exhausted: true,
                },
                TypeTarget::Candidates {
                    targets,
                    complete: true,
                    exhausted: false,
                } if targets.len() >= 2 => TypeTarget::Candidates {
                    targets,
                    complete: true,
                    exhausted: false,
                },
                _ => TypeTarget::Unknown,
            },
        });
    }
    let typing_symbol = matches!(
        nominal,
        Symbol::Type(TypeTarget::Builtin(name)) if name == clean_base
    ) || matches!(
        nominal,
        Symbol::Type(TypeTarget::External { module, .. })
            if matches!(module.as_str(), "typing" | "typing_extensions" | "collections.abc")
    );
    if clean_base == "Callable" && typing_symbol {
        if let [_, ret] = args {
            let ret_target = resolve_type(owner, ret, at, context, SemanticLimits::default());
            let inner = if matches!(ret_target, TypeTarget::Ambiguous) {
                TypeTarget::Unknown
            } else {
                ret_target
            };
            return Some(TypeTarget::CallableReturn(Box::new(inner)));
        }
        return Some(TypeTarget::Unknown);
    }
    if matches!(clean_base, "type" | "Type")
        && (typing_symbol || (clean_base == "type" && matches!(nominal, Symbol::Unknown)))
    {
        if let [inner] = args {
            let inner_target = resolve_type(owner, inner, at, context, SemanticLimits::default());
            if !matches!(inner_target, TypeTarget::Unknown | TypeTarget::Ambiguous) {
                return Some(TypeTarget::ClassType(Box::new(inner_target)));
            }
        }
        return Some(TypeTarget::Unknown);
    }
    if let Symbol::Type(TypeTarget::Local(node)) = nominal {
        let is_generic = node.kind == crate::models::tags::NodeKind::Class
            && node.detail_entry(crate::models::details::NodeDetailKey::BASES).as_str().is_some_and(|bases| {
                bases.contains("Generic") || bases.contains("Protocol") || bases.contains('[')
            });
        if is_generic {
            return Some(TypeTarget::Local(node));
        }
        return Some(TypeTarget::Unknown);
    }
    None
}

pub(in crate::linker) fn has_proven_type_checking(
    path: &str,
    facts: &Facts,
    modules_by_namespace: &ModulesByNamespace<'_>,
    dependencies: &DependencyRegistry,
    family: LanguageFamily,
    interner: &LinkerInterner<'_>,
    node_rebindings: &NodeBindingIndex,
) -> bool {
    let rebound = facts
        .nodes
        .iter()
        .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.path == path)
        .is_some_and(|module| {
            module.rebinding_names().any(|name| name == "TYPE_CHECKING")
                || super::super::typescript::resolution::has_indexed_name(
                    interner,
                    node_rebindings,
                    module,
                    "TYPE_CHECKING",
                )
        });
    if rebound {
        return false;
    }

    let (workspace, _) = crate::languages::workspace_path(path);
    let is_shadowed = |module_name: &str| {
        modules_by_namespace
            .get(&family)
            .and_then(|namespaces| namespaces.get(module_name))
            .is_some_and(|modules| {
                modules.iter().any(|module| {
                    crate::languages::workspace_path(&module.path).0 == workspace
                })
            })
    };

    facts.references.iter().any(|reference| {
        if reference.kind != "imports" {
            return false;
        }
        let proves = |module: &str| {
            (module == "typing" && !is_shadowed("typing"))
                || (module == "typing_extensions"
                    && !is_shadowed("typing_extensions")
                    && (dependencies.declares_for_path(family, path, "typing-extensions")
                        || dependencies.declares_for_path(family, path, "typing_extensions")))
        };
        if reference.expression == "TYPE_CHECKING" {
            return reference.module.as_deref().is_some_and(proves);
        }
        reference
            .module
            .as_deref()
            .is_some_and(|module| reference.expression == module && proves(module))
    })
}

pub(in crate::linker) fn type_only_import_is_unproven(
    profile: &dyn LanguageProfile,
    path: &str,
    facts: &Facts,
    modules_by_namespace: &ModulesByNamespace<'_>,
    dependencies: &DependencyRegistry,
    interner: &LinkerInterner<'_>,
    node_rebindings: &NodeBindingIndex,
) -> bool {
    profile.id() == "python"
        && !has_proven_type_checking(
            path,
            facts,
            modules_by_namespace,
            dependencies,
            profile.family(),
            interner,
            node_rebindings,
        )
}

pub(in crate::linker) fn dynamic_type_alias(
    language: &str,
    module: &str,
    expression: &str,
) -> bool {
    language == "python" && matches!(module, "typing" | "typing_extensions") && expression == "Any"
}

pub(in crate::linker) fn logging_factory_import_is_safe(
    module: &Node,
    alias: &str,
    position: SourcePosition,
) -> bool {
    if module.language != crate::models::tags::LanguageId::Python {
        return false;
    }
    module.detail_entry(crate::models::details::NodeDetailKey::PYTHON_LOGGING_FACTORIES)
        .as_array()
        .is_some_and(|entries| {
            let mut matching = entries.iter().filter(|entry| {
                entry["alias"].as_str() == Some(alias)
                    && entry["line"].as_u64() == Some(position.line as u64)
                    && entry["column"].as_u64() == Some(position.column as u64)
            });
            matching.next().is_some_and(|entry| {
                entry.as_object().is_some_and(|object| object.len() == 4)
                    && entry["safe"].as_bool() == Some(true)
            }) && matching.next().is_none()
        })
}
impl<'a> crate::linker::semantic_context::context::Context<'a> {
    pub(in crate::linker) fn python_super_member(&self, owner: &Node, member: &str) -> crate::linker::value_flow::Symbol<'a> {
        use crate::linker::value_flow::Symbol;
        if owner.language != crate::models::tags::LanguageId::Python
            || !owner.detail_get(crate::models::details::NodeDetailKey::RECEIVER_NAME)
                .and_then(|value| value.as_str()).is_some_and(|name| matches!(name, "self" | "cls"))
        {
            return Symbol::Unknown;
        }
        let Some((class_qual, _)) = owner.qualname.rsplit_once('.') else { return Symbol::Unknown; };
        let Some(class) = self.nodes_in_path(class_qual, &owner.path)
            .find(|node| node.kind == crate::models::tags::NodeKind::Class) else { return Symbol::Unknown; };
        let mut scope = owner.qualname.as_str();
        while !scope.is_empty() {
            if self.nodes_in_path(scope, &owner.path).any(|node| {
                node.binding_names().any(|name| name == "super")
                    || self.node_has_rebinding(node, "super")
                    || self.imported_alias(node, "super").is_some()
            }) { return Symbol::Unknown; }
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        match self.lookup_language_member("python", class, member, true, owner) {
            crate::languages::profile::LanguageMemberResult::Local(node) => Symbol::Callable(node),
            _ => Symbol::Unknown,
        }
    }
}
