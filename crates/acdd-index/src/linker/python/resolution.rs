use super::super::resolution::input::ResolutionInput;
use super::super::semantic_context::LanguageMemberLookup;
use super::super::typescript::resolution::{
    has_indexed_name, has_indexed_symbol, has_indexed_symbol_id,
};
use super::receivers;
use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::LanguageProfile;
use crate::models::{Node, ReceiverHint, Reference};
use hashbrown::{HashMap, HashSet};

pub(in crate::linker) fn nullable_parameter_wrapper<F>(
    profile: Option<&dyn LanguageProfile>,
    raw_annotation: &str,
    node: &Node,
    references: &[Reference],
    external_import_line: F,
) -> bool
where
    F: FnOnce(&str, &Node) -> Option<usize>,
{
    if !profile.is_some_and(|profile| profile.id() == "python") {
        return false;
    }
    let Some(annotation) = crate::linker::types::receiver_type_name(raw_annotation) else {
        return false;
    };
    let (head, member) = annotation.split_once('.').unwrap_or((&annotation, ""));
    let Some(import_line) = external_import_line(head, node) else {
        return false;
    };
    if import_line > node.line {
        return false;
    }
    references.iter().any(|reference| {
        if reference.kind != "imports"
            || reference.line != import_line
            || reference.alias.as_deref() != Some(head)
        {
            return false;
        }
        let Some(module @ ("typing" | "typing_extensions")) = reference.module.as_deref() else {
            return false;
        };
        match (member, reference.expression.as_str()) {
            ("", "Optional" | "Union") => true,
            ("Optional" | "Union", expression) if expression == module => true,
            _ => false,
        }
    })
}

pub(in crate::linker) fn dynamic_type_aliases<'ctx, 'refs: 'ctx, 'nodes: 'ctx>(
    profile: Option<&dyn LanguageProfile>,
    references: &'refs [Reference],
    by_id: &HashMap<SymbolId, &'nodes Node>,
    interner: &LinkerInterner<'_>,
    file_module: &'ctx str,
) -> HashMap<&'ctx str, HashSet<&'ctx str>> {
    let mut aliases = HashMap::new();
    if !profile.is_some_and(|profile| profile.id() == "python") {
        return aliases;
    }
    for reference in references.iter().filter(|reference| {
        reference.kind == "imports"
            && reference.expression == "Any"
            && matches!(
                reference.module.as_deref(),
                Some("typing" | "typing_extensions")
            )
    }) {
        let scope = interner
            .lookup_node(by_id, reference.source.as_str())
            .map_or(file_module, |node| node.qualname.as_str());
        aliases
            .entry(scope)
            .or_default()
            .insert(reference.alias.as_deref().unwrap_or("Any"));
    }
    aliases
}

pub(in crate::linker) fn is_dynamic_type_alias<'scope>(
    name: &str,
    owner: &Node,
    aliases: &HashMap<&'scope str, HashMap<&str, Vec<&Node>>>,
    dynamic_aliases: &HashMap<&'scope str, HashSet<&str>>,
) -> bool {
    let mut scope = owner
        .qualname
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    loop {
        if aliases
            .get(scope)
            .is_some_and(|entries| entries.contains_key(name))
        {
            return dynamic_aliases
                .get(scope)
                .is_some_and(|names| names.contains(name));
        }
        if scope.is_empty() {
            return false;
        }
        scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
}

pub(in crate::linker) fn is_dynamic_typing_annotation<'scope>(
    name: &str,
    member: &str,
    owner: &Node,
    aliases: &HashMap<&'scope str, HashMap<&str, Vec<&Node>>>,
    dynamic_aliases: &HashMap<&'scope str, HashSet<&str>>,
) -> bool {
    matches!(name, "typing" | "typing_extensions")
        && (member == "Any" || is_dynamic_type_alias(name, owner, aliases, dynamic_aliases))
}

pub(in crate::linker) fn path_constructor_aliases<'ctx, 'refs: 'ctx, 'nodes: 'ctx>(
    profile: Option<&dyn LanguageProfile>,
    references: &'refs [Reference],
    by_id: &HashMap<SymbolId, &'nodes Node>,
    interner: &LinkerInterner<'_>,
    file_module: &'ctx str,
) -> HashMap<(&'ctx str, &'ctx str), &'ctx Reference> {
    if !profile.is_some_and(|profile| profile.id() == "python") {
        return HashMap::new();
    }
    references
        .iter()
        .filter(|reference| {
            reference.kind == "imports" && reference.module.as_deref() == Some("pathlib")
        })
        .filter_map(|reference| {
            let alias = reference.alias.as_deref()?;
            let scope = interner
                .lookup_node(by_id, reference.source.as_str())
                .map_or(file_module, |node| node.qualname.as_str());
            Some(((scope, alias), reference))
        })
        .collect()
}

pub(in crate::linker) fn pathlib_member(
    profile: Option<&dyn LanguageProfile>,
    origin: &str,
    imported_expression: &str,
    tail: &str,
    member: &str,
) -> Option<&'static str> {
    if origin != "pathlib" {
        return None;
    }
    let class = if imported_expression == "pathlib" {
        tail
    } else if tail.is_empty() {
        imported_expression
    } else {
        return None;
    };
    let canonical = super::semantic_context::canonical_pathlib_class(class)?;
    profile
        .filter(|profile| profile.id() == "python")
        .is_some_and(|profile| profile.builtin_member(canonical, member))
        .then_some(canonical)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::linker) fn imported_module_member<'a>(
    profile: Option<&dyn LanguageProfile>,
    module: &'a Node,
    tail: &str,
    node_rebindings: &crate::linker::types::NodeBindingIndex,
    interner: &LinkerInterner<'_>,
    by_qual: &HashMap<&'a str, Vec<&'a Node>>,
    lookup_key: &mut String,
    member_lookup: &LanguageMemberLookup<'_, 'a>,
) -> Option<receivers::PythonReceiverMember<'a>> {
    if !profile.is_some_and(|profile| profile.id() == "python")
        || module.kind != crate::models::tags::NodeKind::Module
        || module.language != crate::models::tags::LanguageId::Python
    {
        return None;
    }
    let (class_name, member) = tail.split_once('.')?;
    if member.contains('.') || has_indexed_name(interner, node_rebindings, module, class_name) {
        return None;
    }

    let declarations = by_qual
        .get(crate::linker::types::qualified(
            lookup_key,
            &module.qualname,
            class_name,
        ))?
        .iter()
        .copied()
        .filter(|candidate| candidate.path == module.path);
    let mut declarations = declarations;
    let (Some(class), None) = (declarations.next(), declarations.next()) else {
        return None;
    };
    (class.kind == crate::models::tags::NodeKind::Class)
        .then(|| python_member(member_lookup, class, member, false))
}

fn python_member<'a>(
    member_lookup: &LanguageMemberLookup<'_, 'a>,
    class: &Node,
    member: &str,
    super_call: bool,
) -> super::receivers::PythonReceiverMember<'a> {
    match member_lookup.lookup_member("python", class, member, super_call, class) {
        crate::languages::profile::LanguageMemberResult::Local(node) => {
            super::receivers::PythonReceiverMember::Local(node)
        }
        crate::languages::profile::LanguageMemberResult::External {
            module,
            import_line,
            provider,
            hierarchy,
        } => super::receivers::PythonReceiverMember::External(
            module,
            import_line,
            provider,
            hierarchy,
        ),
        crate::languages::profile::LanguageMemberResult::Ambiguous => {
            super::receivers::PythonReceiverMember::Ambiguous
        }
        _ => super::receivers::PythonReceiverMember::Unknown,
    }
}

/// Borrowed Python view of the shared resolution inputs needed by Python member lookup.
/// The resolution phase builds it from its existing `FileResolver` field borrows.
pub(in crate::linker) struct PythonResolutionView<'ctx, 'a, 'keys> {
    pub(in crate::linker) input: &'ctx super::super::resolution::input::ResolutionInput<'a>,
    pub(in crate::linker) path: &'ctx str,
    pub(in crate::linker) file_module: &'ctx str,
    pub(in crate::linker) aliases: &'ctx HashMap<&'keys str, HashMap<&'a str, Vec<&'a Node>>>,
    pub(in crate::linker) alias_import_positions:
        &'ctx HashMap<(&'keys str, &'keys str), crate::semantic::SourcePosition>,
    pub(in crate::linker) member_lookup: &'ctx LanguageMemberLookup<'a, 'a>,
}

fn is_super_shadowed<'a>(
    input: &ResolutionInput<'a>,
    path: &str,
    owner: Option<&Node>,
    aliases: &HashMap<&str, HashMap<&str, Vec<&'a Node>>>,
) -> bool {
    let super_id = input.interner.lookup_symbol("super");
    owner.is_some_and(|owner| {
        let mut scope = owner.qualname.as_str();
        while !scope.is_empty() {
            if super_id.is_some_and(|super_id| {
                input.by_qual.get(scope).is_some_and(|nodes| {
                    nodes.iter().any(|node| {
                        node.path == path
                            && has_indexed_symbol(
                                input.interner,
                                input.node_bindings,
                                node,
                                super_id,
                            )
                    })
                })
            }) || aliases
                .get(scope)
                .is_some_and(|bindings| bindings.contains_key("super"))
            {
                return true;
            }
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        false
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_python_call_result_member<
    'ctx,
    'a,
    'keys,
    'result,
    'aliases,
    'external,
    AliasFor,
    ExternalFor,
>(
    context: &PythonResolutionView<'ctx, 'a, 'keys>,
    profile: &'static dyn crate::languages::LanguageProfile,
    owner_and_id: (Option<crate::interner::SymbolId>, Option<&'a Node>),
    reference: &Reference,
    callee: &str,
    member: &str,
    path_constructor_aliases: &HashMap<(&str, &str), &Reference>,
    candidates: &mut Vec<&'a Node>,
    lookup_key: &mut String,
    alias_for: &mut AliasFor,
    external_for: &mut ExternalFor,
) -> receivers::PythonReceiverMember<'result>
where
    'a: 'aliases,
    'a: 'result,
    'external: 'result,
    AliasFor: for<'name, 'owner> FnMut(
        &'name str,
        Option<&'owner Node>,
        bool,
    ) -> Option<&'aliases Vec<&'a Node>>,
    ExternalFor: for<'name, 'owner> FnMut(
        &'name str,
        Option<&'owner Node>,
        bool,
    ) -> Option<&'external (String, usize)>,
{
    let input = context.input;
    let path = context.path;
    let (_, owner) = owner_and_id;
    let (head, tail) = callee.split_once('.').unwrap_or((callee, ""));
    let head_id = input.interner.lookup_symbol(head);
    let mut scope = owner.map_or(context.file_module, |owner| owner.qualname.as_str());
    let mut constructor_shadowed = false;
    candidates.clear();
    while !scope.is_empty() {
        if head_id.is_some_and(|head_id| {
            input.by_qual.get(scope).is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node.path == path
                        && has_indexed_symbol(input.interner, input.node_bindings, node, head_id)
                })
            })
        }) {
            constructor_shadowed = true;
            break;
        }
        if let Some(nodes) = input
            .by_qual
            .get(super::super::qualified(lookup_key, scope, callee))
        {
            candidates.extend(nodes.iter().copied().filter(|node| node.path == path));
        }
        if crate::linker::types::shadowed_by_late_import(
            context.alias_import_positions,
            scope,
            head,
            reference.line,
            candidates,
        ) {
            constructor_shadowed = true;
            break;
        }
        if !candidates.is_empty()
            || context
                .aliases
                .get(scope)
                .is_some_and(|entries| entries.contains_key(head))
        {
            break;
        }
        scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
    if candidates.is_empty()
        && !constructor_shadowed
        && context
            .aliases
            .get(scope)
            .is_some_and(|entries| entries.contains_key(head))
    {
        if let Some(imported) = alias_for(head, owner, false) {
            if tail.is_empty() {
                candidates.extend_from_slice(imported);
            } else {
                for target in imported {
                    if let Some(nodes) = input.by_qual.get(super::super::qualified(
                        lookup_key,
                        &target.qualname,
                        tail,
                    )) {
                        candidates.extend(
                            nodes
                                .iter()
                                .copied()
                                .filter(|node| node.path == target.path),
                        );
                    }
                }
            }
        }
    }
    candidates.sort_by_key(|node| &node.id);
    candidates.dedup_by_key(|node| &node.id);
    if let [class] = candidates.as_slice() {
        if class.kind == crate::models::tags::NodeKind::Class {
            return python_member(context.member_lookup, class, member, false);
        }
    }
    if candidates.is_empty() && !constructor_shadowed {
        if let Some((origin, import_line)) =
            external_for(head, owner, false).filter(|(_, line)| *line <= reference.line)
        {
            if let Some(import) = path_constructor_aliases
                .get(&(scope, head))
                .filter(|import| import.line == *import_line)
            {
                if let Some(canonical) =
                    pathlib_member(Some(profile), origin, &import.expression, tail, member)
                {
                    return receivers::PythonReceiverMember::External(
                        origin.as_str(),
                        *import_line,
                        Some(canonical),
                        None,
                    );
                }
            }
        }
    }
    receivers::PythonReceiverMember::Unknown
}

/// Computes Python-only dynamic receiver evidence before the shared dynamic fallback runs.
/// The caller invokes this at the existing dynamic-reference position; `Unknown` and
/// `literal_builtin == false` leave the reference to the shared status/evidence handling.
pub(in crate::linker) fn dynamic_receiver_member<
    'ctx,
    'a,
    'keys,
    'result,
    'aliases,
    'external,
    AliasFor,
    ExternalFor,
>(
    context: &PythonResolutionView<'ctx, 'a, 'keys>,
    reference: &Reference,
    path_constructor_aliases: &HashMap<(&str, &str), &Reference>,
    candidates: &mut Vec<&'a Node>,
    lookup_key: &mut String,
    mut alias_for: AliasFor,
    mut external_for: ExternalFor,
) -> (receivers::PythonReceiverMember<'result>, bool)
where
    'a: 'aliases,
    'a: 'result,
    'external: 'result,
    AliasFor: for<'name, 'owner> FnMut(
        &'name str,
        Option<&'owner Node>,
        bool,
    ) -> Option<&'aliases Vec<&'a Node>>,
    ExternalFor: for<'name, 'owner> FnMut(
        &'name str,
        Option<&'owner Node>,
        bool,
    ) -> Option<&'external (String, usize)>,
{
    let input = context.input;
    let path = context.path;
    let Some(profile) = input
        .profiles
        .get(path)
        .copied()
        .filter(|profile| profile.id() == "python")
    else {
        return (receivers::PythonReceiverMember::Unknown, false);
    };
    let owner_id = input.interner.lookup_symbol(reference.source.as_str());
    let owner = owner_id.and_then(|id| input.by_id.get(&id).copied());
    let by_qual = input.by_qual;
    let node_rebindings = input.node_rebindings;
    let aliases = context.aliases;
    let member_lookup = context.member_lookup;

    let mut computed: receivers::PythonReceiverMember<'result> =
        receivers::PythonReceiverMember::Unknown;
    let mut literal_builtin = false;
    match reference.receiver_hint.as_ref() {
        Some(ReceiverHint::StringLiteral { member }) => {
            literal_builtin = profile.builtin_member("str", member);
        }
        Some(ReceiverHint::Super { member }) => {
            if !is_super_shadowed(input, path, owner, aliases) {
                if let Some((owner_id, owner)) = owner_id.zip(owner).filter(|&(owner_id, owner)| {
                    owner
                        .detail_entry(crate::models::details::NodeDetailKey::RECEIVER_NAME)
                        .as_str()
                        .is_some_and(|name| {
                            matches!(name, "self" | "cls")
                                && !input.interner.lookup_symbol(name).is_some_and(|name_id| {
                                    has_indexed_symbol_id(node_rebindings, owner_id, name_id)
                                })
                        })
                }) {
                    let _ = owner_id;
                    if let Some((scope, _)) = owner.qualname.rsplit_once('.') {
                        if let Some([class]) = by_qual.get(scope).map(Vec::as_slice) {
                            if class.kind == crate::models::tags::NodeKind::Class
                                && class.path == path
                            {
                                computed = python_member(member_lookup, class, member, true);
                            }
                        }
                    }
                }
            }
        }
        Some(
            ReceiverHint::CallResult { callee, member }
            | ReceiverHint::TypedCallResult {
                value: crate::semantic::ValueExpr::Call { callee, site: None },
                member,
            },
        ) => {
            computed = resolve_python_call_result_member(
                context,
                profile,
                (owner_id, owner),
                reference,
                callee,
                member,
                path_constructor_aliases,
                candidates,
                lookup_key,
                &mut alias_for,
                &mut external_for,
            );
        }
        _ => {}
    }

    (computed, literal_builtin)
}

/// Returns whether the first visible Python import is available at this reference position.
/// Non-Python references and scopes with no matching import retain the shared resolver's fallthrough.
pub(in crate::linker) fn import_visible_at<'ctx, 'a, 'keys>(
    context: &crate::linker::traits::ImportVisibility<'a, 'ctx, 'keys>,
    reference: &Reference,
    definition_scope: bool,
) -> bool {
    let path = context.path;
    if !context
        .input
        .profiles
        .get(path)
        .is_some_and(|profile| profile.id() == "python")
    {
        return true;
    }
    let owner = context
        .input
        .interner
        .lookup_node(context.input.by_id, reference.source.as_str());
    let mut import_scope = owner
        .map(|node| crate::linker::resolution::context::reference_scope(node, path))
        .unwrap_or("");
    let first = reference
        .expression
        .split_once('.')
        .map_or(reference.expression.as_str(), |(first, _)| first);
    let at = reference.position();
    let by_qual = context.input.by_qual;
    let aliases = context.aliases;
    let alias_import_positions = context.alias_import_positions;
    if definition_scope {
        import_scope = import_scope
            .rsplit_once('.')
            .map_or("", |(parent, _)| parent);
    }
    loop {
        let class_scope = !definition_scope
            && owner.is_some_and(|node| matches!(node.kind.as_str(), "function" | "method"))
            && by_qual.get(import_scope).is_some_and(|nodes| {
                nodes
                    .iter()
                    .any(|node| node.path == path && matches!(node.kind.as_str(), "class" | "impl"))
            });
        if !class_scope
            && aliases
                .get(import_scope)
                .is_some_and(|entries| entries.contains_key(first))
        {
            return alias_import_positions
                .get(&(import_scope, first))
                .is_none_or(|position| *position <= at);
        }
        if import_scope.is_empty() {
            return true;
        }
        import_scope = import_scope
            .rsplit_once('.')
            .map_or("", |(parent, _)| parent);
    }
}

/// Looks up the Python member for a typed, implicit, or imported class receiver.
/// A returned `Some(Unknown)` is a handled lookup; `None` preserves shared resolution fallback.
pub(in crate::linker) fn implicit_receiver_member<'ctx, 'a, 'keys>(
    context: &PythonResolutionView<'ctx, 'a, 'keys>,
    reference: &Reference,
    typed_receiver: Option<crate::linker::types::ReceiverType<'a>>,
    known_receiver: bool,
    shadowed: bool,
    imported: Option<&Vec<&'a Node>>,
    lookup_key: &mut String,
) -> Option<receivers::PythonReceiverMember<'a>> {
    let input = context.input;
    let path = context.path;
    let file_module = context.file_module;
    let profile = input.profiles.get(path).copied();
    let owner = input
        .interner
        .lookup_node(input.by_id, reference.source.as_str());
    let by_qual = input.by_qual;
    let member_lookup = context.member_lookup;
    let (first, tail) = reference
        .expression
        .split_once('.')
        .unwrap_or((&reference.expression, ""));
    let kind = reference.kind.as_str();
    let receiver_class =
        if profile.is_some_and(|profile| profile.id() == "python") && !tail.is_empty() {
            if let Some(crate::linker::types::ReceiverType::Local(class)) = typed_receiver {
                Some(class)
            } else if known_receiver {
                owner
                    .and_then(|owner| owner.qualname.rsplit_once('.'))
                    .and_then(|(scope, _)| by_qual.get(scope))
                    .and_then(|nodes| {
                        let mut classes = nodes.iter().copied().filter(|node| {
                            node.path == path && node.kind == crate::models::tags::NodeKind::Class
                        });
                        let class = classes.next()?;
                        classes.next().is_none().then_some(class)
                    })
            } else if !shadowed {
                let classes = imported.map(Vec::as_slice).unwrap_or_else(|| {
                    by_qual
                        .get(crate::linker::types::qualified(
                            lookup_key,
                            file_module,
                            first,
                        ))
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                });
                match classes {
                    [class] if class.kind == crate::models::tags::NodeKind::Class => Some(*class),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        };
    receiver_class
        .filter(|_| matches!(kind, "calls" | "handles" | "references"))
        .map(|class| python_member(member_lookup, class, tail, false))
}
