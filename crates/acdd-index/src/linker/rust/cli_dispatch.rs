use hashbrown::HashMap;

use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::dependency_registry::DependencyRegistry;
use crate::languages::manifests::syntax::SyntaxOperation;
use crate::languages::profile::LanguageMemberResult;
use crate::languages::rust::cli_registration::{
    CliDispatchMetadata, CliParserMetadata, CliSubcommandMetadata,
};
use crate::linker::semantic_context;
use crate::linker::value_flow::{SemanticResolver, Symbol, SymbolRole, TypeTarget};
use crate::models::{
    compact_graph::CompactGraph as Graph,
    details::NodeDetailKey,
    tags::{EdgeConfidence, EdgeKind, LanguageId, NodeKind},
    Facts, Node,
};
use crate::semantic::SourcePosition;

pub(in crate::linker) fn compose_file_relations<'a>(
    path: &str,
    facts: &'a Facts,
    dependencies: &DependencyRegistry,
    semantic_context: &semantic_context::Context<'a>,
    interner: &LinkerInterner<'a>,
    by_id: &HashMap<SymbolId, &'a Node>,
    graph: &mut Graph,
) {
    let mut rules_cache = None;
    for reg in facts.nodes.iter().filter(|node| {
        node.kind == NodeKind::CliCommandRegistration && node.language == LanguageId::Rust
    }) {
        let Some(dispatch) = reg
            .detail_get(NodeDetailKey::CLI_COMMAND_DISPATCH)
            .and_then(|v| serde_json::from_value::<CliDispatchMetadata>(v.clone()).ok())
        else {
            continue;
        };
        let rules = rules_cache.get_or_insert_with(|| dependencies.language_context_for_path(path));
        let active_rule = rules.syntax_rules.iter().any(|rule| {
            rule.language == "rust"
                && rule.pack == dispatch.pack
                && rule.id == dispatch.rule
                && matches!(
                    &rule.operation,
                    SyntaxOperation::CommandDispatch { parse_method, .. }
                        if parse_method == &dispatch.parse_method
                )
                && rules.is_syntax_rule_active(rule, path, &facts.activation_signals)
        });
        if !active_rule {
            continue;
        }
        let Some(owner_fn) = interner.lookup_node(by_id, &dispatch.owner_id) else {
            continue;
        };
        let pos = SourcePosition {
            line: dispatch.handler_line,
            column: 0,
        };
        let Symbol::Type(TypeTarget::Local(parser_struct)) =
            semantic_context.resolve_symbol(owner_fn, &dispatch.parser_type, pos, SymbolRole::Type)
        else {
            continue;
        };
        if parser_struct.kind != NodeKind::Struct {
            continue;
        }
        let Some(parser_meta) = parser_struct
            .detail_get(NodeDetailKey::CLI_COMMAND_PARSER)
            .and_then(|v| serde_json::from_value::<Vec<CliParserMetadata>>(v.clone()).ok())
            .and_then(|entries| {
                entries.into_iter().find(|entry| {
                    entry.pack == dispatch.pack
                        && entry.rule == dispatch.rule
                        && entry.parse_method == dispatch.parse_method
                        && entry.subcommand_field == dispatch.subcommand_field
                })
            })
        else {
            continue;
        };
        let overridden = semantic_context
            .member_state("rust")
            .is_some_and(|members| {
                match members.lookup_member(parser_struct, &dispatch.parse_method, true, owner_fn) {
                    LanguageMemberResult::Local(method) => {
                        method.detail_entry(NodeDetailKey::SYNTHESIZED) != true
                    }
                    LanguageMemberResult::Candidates(methods) => methods
                        .iter()
                        .any(|method| method.detail_entry(NodeDetailKey::SYNTHESIZED) != true),
                    LanguageMemberResult::DynamicDispatch(_, _)
                    | LanguageMemberResult::Ambiguous => true,
                    LanguageMemberResult::External { .. } | LanguageMemberResult::Unknown => false,
                }
            });
        if overridden {
            continue;
        }
        let parser_pos = SourcePosition {
            line: parser_struct.line,
            column: 0,
        };
        let Symbol::Type(TypeTarget::Local(expected_enum)) = semantic_context.resolve_symbol(
            parser_struct,
            &parser_meta.subcommand_type,
            parser_pos,
            SymbolRole::Type,
        ) else {
            continue;
        };
        let Symbol::Type(TypeTarget::Local(dispatch_enum)) =
            semantic_context.resolve_symbol(owner_fn, &dispatch.enum_type, pos, SymbolRole::Type)
        else {
            continue;
        };
        if expected_enum.kind != NodeKind::Enum || expected_enum.id != dispatch_enum.id {
            continue;
        }
        let valid_subcommand = expected_enum
            .detail_get(NodeDetailKey::CLI_COMMAND_SUBCOMMAND)
            .and_then(|v| serde_json::from_value::<Vec<CliSubcommandMetadata>>(v.clone()).ok())
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.pack == dispatch.pack && entry.rule == dispatch.rule)
            });
        if !valid_subcommand {
            continue;
        }
        let variant_qual = format!("{}.{}", expected_enum.qualname, dispatch.variant);
        let has_variant = semantic_context
            .nodes_in_path(&variant_qual, &expected_enum.path)
            .any(|node| {
                node.kind == NodeKind::Constant
                    && node.detail_entry(NodeDetailKey::RUST_ENUM_VARIANT) == true
            });
        if !has_variant {
            continue;
        }
        let Symbol::Callable(handler_fn) =
            semantic_context.resolve_symbol(owner_fn, &dispatch.handler, pos, SymbolRole::Callable)
        else {
            continue;
        };
        if !matches!(handler_fn.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        graph.push_edge(
            path,
            &reg.id,
            &handler_fn.id,
            EdgeKind::Handles,
            dispatch.handler_line,
            format!(
                "{}::{} -> {}",
                dispatch.enum_type, dispatch.variant, dispatch.handler
            ),
            EdgeConfidence::Exact,
        );
    }
}
