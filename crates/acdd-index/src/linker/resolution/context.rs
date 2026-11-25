use super::input::ResolutionInput;
use crate::languages::profile::ProfileLinker;
use crate::languages::{self, LanguageProfile};
use crate::linker::{semantic_context, value_flow};
use crate::models::{compact_graph::CompactGraph as Graph, Facts, Node, Reference};
use hashbrown::{HashMap, HashSet};
use std::borrow::Cow;
use std::path::Path;

pub(in crate::linker) fn emit_inferred(
    graph: &mut Graph,
    path: &str,
    reference: &Reference,
    result: semantic_context::Member<'_>,
    builtin_origin: Option<&str>,
) {
    let (status, evidence): (&str, Cow<'static, str>) = match &result {
        semantic_context::Member::Local(node) => (
            "resolved",
            format!("statically inferred value receiver: {}", node.qualname).into(),
        ),
        semantic_context::Member::ConfiguredExternal {
            authority,
            origin,
            evidence,
        } => {
            let authority = match authority {
                value_flow::ConfiguredReceiverAuthority::Hierarchy { pack, rule } => format!(
                    "pack {pack}; hierarchy provider {}; ordered bases {}",
                    rule.provider,
                    rule.bases.join(",")
                ),
                value_flow::ConfiguredReceiverAuthority::Nominal { pack, rule_id, .. } => {
                    format!("pack {pack}; receiver rule {rule_id}")
                }
            };
            let origin = match origin {
                value_flow::TypeTarget::External {
                    module,
                    import_line,
                } => format!("external import {module} at line {import_line}"),
                value_flow::TypeTarget::Local(node) | value_flow::TypeTarget::Callable(node) => {
                    format!("local receiver {} in {} at line {}", node.qualname, node.path, node.line)
                }
                value_flow::TypeTarget::ConfiguredReceiver { evidence, .. } => {
                    format!("configured receiver {evidence}")
                }
                _ => "non-import receiver without an exact local declaration".to_owned(),
            };
            let language =
                languages::for_path(Path::new(path)).map_or("unknown", |profile| profile.id());
            (
                "external",
                format!(
                    "finite {language} configured member {}; {authority}; receiver origin {origin}; configuration {evidence}",
                    reference.expression
                )
                .into(),
            )
        }
        semantic_context::Member::External(module, line, Some(provider)) => {
            let member = reference.expression.rsplit('.').next().unwrap_or_default();
            let profile = languages::for_path(Path::new(path));
            let diagnostic_marker = if profile.is_some_and(|profile| {
                profile.linker().framework_magic_diagnostic(profile, module)
            }) {
                "; diagnostic_reason=framework_magic"
            } else {
                ""
            };
            let label = profile.map_or("finite framework member", |profile| {
                profile.linker().framework_member_label()
            });
            (
                "external",
                format!(
                    "{label} from {provider} via external import {module} at line {line}; resolved member {provider}.{member}; receiver origin statically inferred{diagnostic_marker}"
                )
                .into(),
            )
        }
        semantic_context::Member::External(module, line, None) => (
            "external",
            format!("call through external import {module} at line {line}; receiver origin statically inferred").into(),
        ),
        semantic_context::Member::Builtin => match builtin_origin {
            Some(origin) => (
                "external",
                format!("{origin} built-in: {}", reference.expression).into(),
            ),
            None => (
                "resolved",
                format!("standard library or built-in callee: {}", reference.expression).into(),
            ),
        },
        semantic_context::Member::Ambiguous => (
            "ambiguous",
            "statically inferred receiver has multiple method candidates".into(),
        ),
        semantic_context::Member::Unknown => (
            "unresolved",
            "statically inferred receiver has no verified method candidate".into(),
        ),
    };
    graph.reference_coverage(path, reference, status, evidence);
    if let semantic_context::Member::Local(node) = result {
        graph.reference_edge(
            path,
            reference,
            node.id.clone(),
            crate::models::tags::EdgeKind::parse(&reference.kind),
            crate::models::tags::EdgeConfidence::Inferred,
        );
    }
}

pub(crate) type ExternalAliases<'a> = HashMap<&'a str, Option<(String, usize)>>;

pub(super) fn chained_member_receiver<'a, 'idx>(
    receiver: &'idx value_flow::TypeTarget<'a>,
    fields: &str,
    owner: &'a Node,
    path: &str,
    at: crate::semantic::SourcePosition,
    input: &ResolutionInput<'a>,
    profile: Option<&dyn LanguageProfile>,
) -> Option<Cow<'idx, value_flow::TypeTarget<'a>>> {
    let value_flow = input.value_flow;
    let semantic_context = input.semantic_context;
    let mut receiver = Cow::Borrowed(receiver);
    for field in fields.split('.').filter(|field| !field.is_empty()) {
        receiver = match receiver.as_ref() {
            value_flow::TypeTarget::Local(class) => {
                value_flow.field_type(class, field).map(Cow::Borrowed)
            }
            value_flow::TypeTarget::ConfiguredReceiver { .. }
            | value_flow::TypeTarget::Collection { .. } => {
                match value_flow::SemanticResolver::resolve_member(
                    semantic_context,
                    receiver.as_ref(),
                    field,
                    owner,
                    at,
                ) {
                    value_flow::Symbol::Type(target) => Some(Cow::Owned(target)),
                    _ => None,
                }
            }
            value_flow::TypeTarget::Builtin(name) => profile
                .and_then(|profile| {
                    profile
                        .linker()
                        .resolve_builtin_property(path, name, field, semantic_context)
                })
                .map(Cow::Owned),
            other => profile
                .and_then(|profile| profile.linker().local_receiver_class(other))
                .and_then(|class| {
                    value_flow
                        .field_type(class, field)
                        .map(Cow::Borrowed)
                        .or_else(|| {
                            match value_flow::SemanticResolver::resolve_member(
                                semantic_context,
                                receiver.as_ref(),
                                field,
                                owner,
                                at,
                            ) {
                                value_flow::Symbol::Type(target) => Some(Cow::Owned(target)),
                                _ => None,
                            }
                        })
                }),
        }?;
    }
    Some(receiver)
}

pub(in crate::linker) fn reference_scope<'a>(owner: &'a Node, path: &str) -> &'a str {
    if owner
        .detail_entry(crate::models::details::NodeDetailKey::INCLUDED_FROM)
        .is_string()
        && owner
            .detail_entry(crate::models::details::NodeDetailKey::SOURCE_PATH)
            .as_str()
            == Some(path)
    {
        owner
            .detail_entry(crate::models::details::NodeDetailKey::SOURCE_QUALNAME)
            .as_str()
            .filter(|scope| !scope.is_empty())
            .unwrap_or(owner.qualname.as_str())
    } else {
        owner.qualname.as_str()
    }
}

pub(super) fn document_symbol_candidates<'a>(
    normalized_qualname: &str,
    declared_name: &str,
    source_path: &str,
    target_path: Option<&str>,
    by_qual: &HashMap<&'a str, Vec<&'a Node>>,
    by_name: &HashMap<&'a str, Vec<&'a Node>>,
    by_suffix: &HashMap<&'a str, Vec<&'a Node>>,
) -> Option<Vec<&'a Node>> {
    for tier in [
        by_qual.get(normalized_qualname),
        by_name.get(declared_name),
        by_suffix.get(normalized_qualname),
    ]
    .into_iter()
    .flatten()
    {
        let mut candidates: Option<Vec<&'a Node>> = None;
        for node in tier.iter().copied().filter(|node| {
            !matches!(
                node.kind.as_str(),
                "document" | "invariant" | "component" | "impl"
            ) && target_path.map_or_else(
                || {
                    acdd_core::config::workspace_path(&node.path).0
                        == acdd_core::config::workspace_path(source_path).0
                },
                |path| node.path == path,
            )
        }) {
            candidates.get_or_insert_with(Vec::new).push(node);
        }
        let Some(mut candidates) = candidates else {
            continue;
        };
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        candidates.dedup_by(|left, right| left.id == right.id);
        return Some(candidates);
    }
    None
}

pub(super) struct LinkerContext<'a> {
    pub(super) member_lookup: semantic_context::LanguageMemberLookup<'a, 'a>,
}

pub(super) struct FileResolver<'a, 'file> {
    pub(super) input: &'file ResolutionInput<'a>,
    pub(super) linker: &'file LinkerContext<'a>,
    pub(super) path: &'file str,
    pub(super) path_arc: std::sync::Arc<str>,
    pub(super) facts: &'a Facts,
    pub(super) references: &'file [Reference],
    pub(super) file_module: &'file str,
    pub(super) language_context: &'file languages::LanguageContext,
    pub(super) graph: Graph,
    pub(super) aliases: HashMap<&'file str, HashMap<&'a str, Vec<&'a Node>>>,
    pub(super) external_aliases: HashMap<&'file str, ExternalAliases<'a>>,
    pub(super) type_only_aliases: HashMap<&'file str, HashSet<&'a str>>,
    pub(super) lookup_key: String,
    pub(super) reference_extension:
        Option<Box<dyn crate::linker::traits::ReferenceExtension + 'file>>,
}

impl<'a, 'file> FileResolver<'a, 'file>
where
    'a: 'file,
{
    pub(super) fn new(
        input: &'file ResolutionInput<'a>,
        path: &'file str,
        facts: &'a Facts,
        references: &'file [Reference],
        file_module: &'file str,
        language_context: &'file languages::LanguageContext,
        linker: &'file LinkerContext<'a>,
    ) -> Self {
        let path_arc = std::sync::Arc::<str>::from(path);
        let mut graph = Graph {
            edges: Vec::with_capacity(facts.edges.len()),
            coverage: Vec::with_capacity(references.len()),
            path_arc: Some(std::sync::Arc::clone(&path_arc)),
            path_pool: HashSet::new(),
            node_id_pool: HashSet::new(),
            evidence_pool: HashSet::new(),
        };
        for edge in &facts.edges {
            graph
                .try_push_public_edge(edge)
                .expect("invalid edge tag in linker facts");
        }
        Self {
            input,
            linker,
            path,
            path_arc,
            facts,
            references,
            file_module,
            language_context,
            graph,
            aliases: HashMap::new(),
            external_aliases: HashMap::new(),
            type_only_aliases: HashMap::new(),
            lookup_key: String::new(),
            reference_extension: None,
        }
    }
}
