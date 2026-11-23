pub(crate) mod fingerprints;
/// Low-level AST file extraction entrypoint.
pub mod parse;
pub mod relations;
pub mod route_includes;
pub mod scope_analysis;
pub mod search;
pub mod syntax;
pub mod visitor;
pub(crate) use parse::{
    clear_direct_module_markers, compact_node_details, extract_tree_with_language_context,
    parse_file, parse_file_typed,
};
use crate::languages;
use crate::models::{tags::{LanguageId, NodeKind}, *};
use crate::typed_facts::{FlowStore, TypedFacts};
use anyhow::Result;
pub use search::{
    pattern_symbol_name, search, PatternSearchDiagnostic, SEARCH_MATCH_HORIZON,
};

/// Performs extract.
pub fn extract(path: &str, language: &str, source: &str) -> Result<Facts> {
    Ok(extract_typed(path, language, source)?.into_public())
}

fn is_trivial_source(source: &str, language: &str) -> bool {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return true;
    }
    match language {
        "python" => trimmed.lines().all(|l| {
            let s = l.trim();
            s.is_empty() || (s.starts_with('#') && !s.starts_with("# type:"))
        }),
        "rust" => trimmed.lines().all(|l| {
            let s = l.trim();
            s.is_empty() || (s.starts_with("//") && !s.starts_with("//!") && !s.starts_with("///"))
        }),
        "javascript" | "typescript" => trimmed.lines().all(|l| {
            let s = l.trim();
            s.is_empty() || (s.starts_with("//") && !s.starts_with("///"))
        }),
        _ => false,
    }
}

/// Extracts typed AST facts and framework rules for the given source file.
pub fn extract_typed(path: &str, language: &str, source: &str) -> Result<TypedFacts> {
    let registry = languages::dependency_registry::DependencyRegistry::bundled_framework_rules();
    extract_typed_with_registry(path, language, source, &registry)
}

fn manifest_entrypoint_candidates(
    path: &str,
    language: &str,
    registry: &languages::dependency_registry::DependencyRegistry,
    facts: &mut Facts,
) {
    for entry in registry.manifest_entrypoints() {
        let relative = if entry.scope.is_empty() {
            Some(path)
        } else {
            path.strip_prefix(&entry.scope)
                .and_then(|rest| rest.strip_prefix('/'))
        };
        let Some(relative) = relative else { continue };
        let callable = if entry.python && language == "python" {
            let Some((module, callable)) = entry.target.split_once(':') else {
                continue;
            };
            if module.is_empty()
                || callable.is_empty()
                || !module.split('.').all(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_')
                })
                || !callable.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            let module_path = module.replace('.', "/");
            let source_path = relative.strip_prefix("src/").unwrap_or(relative);
            if source_path != format!("{module_path}.py")
                && source_path != format!("{module_path}/__init__.py")
            {
                continue;
            }
            Some(callable)
        } else if !entry.python && matches!(language, "typescript" | "javascript") {
            let target = entry.target.strip_prefix("./").unwrap_or(&entry.target);
            if target != relative || target.split('/').any(|part| part == "..") {
                continue;
            }
            None
        } else {
            continue;
        };
        let direct_module = callable.is_some_and(|callable| {
            let Some(module) = facts
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Module.as_str())
            else {
                return false;
            };
            let qualified = format!("{}.{}", module.qualname, callable);
            let mut targets = facts.nodes.iter().filter(|node| {
                node.kind == NodeKind::Function.as_str() && node.qualname == qualified
            });
            let Some(target) = targets.next() else {
                return false;
            };
            target.details["entrypoint_direct_module"] == true && targets.next().is_none()
        });
        let id = format!("main-registration:{}:{}:{}", entry.origin, entry.name, path);
        facts.nodes.push(Node {
            native_semantic: None,
            id,
            kind: NodeKind::MainRegistration.as_str().into(),
            name: entry.name.clone(),
            qualname: entry.target.clone(),
            path: path.to_owned(),
            line: 1,
            end_line: 1,
            is_test: is_test(path),
            language: language.into(),
            generated: false,
            details: serde_json::json!({"manifest_origin": entry.origin, "manifest_name": entry.name, "callable": callable, "direct_module": direct_module}),
        });
    }
    if language == LanguageId::Python.as_str() {
        clear_direct_module_markers(facts);
    }
}

/// Extracts typed facts using a language dependency registry.
pub fn extract_typed_with_registry(
    path: &str,
    language: &str,
    source: &str,
    registry: &languages::dependency_registry::DependencyRegistry,
) -> Result<TypedFacts> {
    let language_context = registry.language_context_for_path(path);
    let profile = languages::require(language)?;
    let module = profile.module_name_for_source(path, source);
    let mut facts = Facts::default();
    facts.nodes.push(Node {
        native_semantic: None,
        id: format!("module:{path}"),
        kind: NodeKind::Module.as_str().into(),
        name: path.rsplit('/').next().unwrap_or(path).into(),
        qualname: module.clone(),
        path: path.into(),
        line: 1,
        end_line: source.lines().count().max(1),
        is_test: is_test(path),
        language: language.into(),
        generated: false,
        details: serde_json::Value::Null,
    });

    let bytes = source.len();
    if bytes > 1024 {
        facts.nodes.reserve((bytes / 512).clamp(4, 256));
        facts.edges.reserve((bytes / 256).clamp(8, 512));
        facts.references.reserve((bytes / 256).clamp(8, 512));
    }

    let mut typed = TypedFacts {
        facts,
        flows: FlowStore::default(),
    };
    if is_trivial_source(source, language) {
        profile.finish(&mut typed.facts);
        languages::file_providers(&mut typed.facts, &language_context);
        typed.facts.semantic.mark_complete();
        compact_node_details(&mut typed.facts, path);
        return Ok(typed);
    }
    match language {
        "python" | "rust" | "typescript" | "javascript" => {
            parse_file_typed(
                profile,
                path,
                source,
                &module,
                &mut typed,
                &language_context,
            )?;
            profile.finish(&mut typed.facts);
        }
        #[cfg(feature = "lang-html")]
        "html" => languages::html::extract_typed_with_context(
            path,
            source,
            &module,
            &mut typed,
            &language_context,
            extract_tree_with_language_context,
        )?,
        #[cfg(feature = "lang-vue")]
        "vue" => languages::vue::extract_typed(
            path,
            source,
            &module,
            &mut typed,
            &language_context,
            extract_tree_with_language_context,
        )?,
        _ => {
            parse_file(profile, path, source, &module, &mut typed.facts)?;
            profile.finish(&mut typed.facts);
            for (index, node) in typed.facts.nodes.iter_mut().enumerate() {
                typed.flows.ingest(node);
                typed.facts.semantic.ingest(index, node);
            }
        }
    }
    manifest_entrypoint_candidates(path, language, registry, &mut typed.facts);
    languages::file_providers(&mut typed.facts, &language_context);
    // Only generated declarations retain a parser-owned JSON flow projection.
    // Body scopes publish directly into their canonical native store.
    for index in std::mem::take(&mut typed.facts.pending_flow_nodes) {
        let node = typed
            .facts
            .nodes
            .get_mut(index)
            .expect("generated flow target follows node remapping");
        if typed.flows.state(node).is_none() {
            typed.flows.ingest(node);
        }
    }
    for node in &mut typed.facts.nodes {
        crate::typed_facts::NodeSemanticEntry::ingest(node);
    }
    typed.facts.semantic.mark_complete();
    compact_node_details(&mut typed.facts, path);
    Ok(typed)
}

#[cfg(test)]
mod tests {
    use crate::languages::LanguageProfile;
    #[cfg(feature = "lang-python")]
    #[test]
    fn persistence_type_only_imports_populate_module_ast_facts() {
        let source = "from typing import TYPE_CHECKING\n\nclass PersistenceMixin:\n    if TYPE_CHECKING:\n        from contextunity.router.service.payloads import GraphEntry\n";
        let ast_facts = super::extract("persistence.py", "python", source).unwrap();
        let ast_facts = serde_json::to_value(&ast_facts).unwrap();
        let module = ast_facts["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "module:persistence.py")
            .unwrap();
        let type_only = &module["details"]["value_flow"]["type_only_imports"];
        assert_eq!(type_only[0]["alias"], "GraphEntry");
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn test_ast_parameterless_declaration() {
        let capabilities = crate::languages::rust::RUST.ast_search_capabilities();
        assert!(capabilities.declaration_kinds.contains(&"function_item"));
        let matches = super::search(
            "fn target() { call(value); }\nfn other() {}",
            "lib.rs",
            "rust",
            "fn target",
            10,
        )
        .expect("a Rust function header without parameters is searchable");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["text"], "fn target() { call(value); }");
    }

    #[cfg(feature = "lang-html")]
    #[test]
    fn django_url_tag_emits_named_route_reference() {
        let facts = super::extract("templates/products.html", "html", "{% url 'product-list' %}")
            .unwrap();

        assert!(facts.references.iter().any(|reference| {
            reference.kind == "references_route"
                && reference.expression == "django.route_name.product-list"
        }));
    }
}

#[cfg(all(test, feature = "lang-python"))]
mod legacy_hook_tests {
    use super::visitor::SinglePassExtractor;
    use crate::{
        ast::syntax::ScopeBindings,
        languages::{self, context::FileContext, routes, LanguageProfile, SyntaxContext},
        models::Facts,
        semantic::{BindingFact, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts},
        typed_facts::TypedFacts,
    };
    use anyhow::Result;
    use std::collections::HashMap;
    use tree_sitter::Node as Syntax;

    struct Legacy;
    impl LanguageProfile for Legacy {
        fn id(&self) -> &'static str {
            "legacy-typed"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["py"]
        }
        fn grammar(&self, path: &str) -> tree_sitter::Language {
            languages::require("python").unwrap().grammar(path)
        }
        fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
            (kind == "function_definition").then_some("function")
        }
        fn node_prefix(&self, _: &str) -> &'static str {
            "legacy"
        }
        fn normalize_import(&self, _: &str, _: &str) -> Option<languages::ImportPath> {
            None
        }
        fn prepare(&self, root: Syntax<'_>, _: &str) -> FileContext {
            FileContext {
                root_bindings: Some((
                    root.id(),
                    ScopeBindings {
                        all: vec!["prepared".into()],
                        rebindings: vec!["rebound".into()],
                    },
                )),
                ..Default::default()
            }
        }
        fn declaration(
            &self,
            _: Syntax<'_>,
            _: &str,
            _: &str,
            _: &FileContext,
        ) -> (languages::SymbolMetadata, ValueFlowFacts) {
            (
                languages::SymbolMetadata {
                    is_static: Some(true),
                    return_type: Some(TypeExpr::Named {
                        name: "MetadataReturn".into(),
                    }),
                    ..Default::default()
                },
                {
                    ValueFlowFacts {
                        framework: Some(Box::new(crate::semantic::FrameworkScopeFacts {
                            alias_type: Some(TypeExpr::Named {
                                name: "Alias".into(),
                            }),
                            ..Default::default()
                        })),
                        bindings: vec![BindingFact {
                            name: "local".into(),
                            position: SourcePosition { line: 2, column: 4 },
                            value: ValueExpr::Alias {
                                name: "argument".into(),
                            },
                            conditional: false,
                        }],
                        ..Default::default()
                    }
                },
            )
        }
        fn extract_file(&self, path: &str, source: &str, module: &str, facts: &mut Facts) -> Result<()> {
            super::parse_file(self, path, source, module, facts)
        }
    }

    struct Incremental(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
    impl LanguageProfile for Incremental {
        fn id(&self) -> &'static str {
            "scope-order"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["py"]
        }
        fn grammar(&self, path: &str) -> tree_sitter::Language {
            Legacy.grammar(path)
        }
        fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
            Legacy.symbol_kind(kind)
        }
        fn node_prefix(&self, _: &str) -> &'static str {
            "scope-order"
        }
        fn normalize_import(&self, _: &str, _: &str) -> Option<languages::ImportPath> {
            None
        }
        fn scope_analysis<'source, 'tree>(
            &self,
            node: Syntax<'tree>,
            source: &'source str,
            file: &FileContext,
        ) -> Option<super::scope_analysis::ScopeAnalysis<'source, 'tree>> {
            let inner = languages::require("python")
                .unwrap()
                .scope_analysis(node, source, file)?;
            if node.kind() != "function_definition" {
                return Some(inner);
            }
            let name = node
                .child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap()
                .to_owned();
            let trace = self.0.clone();
            Some(inner.with_finish_hook(move || {
                trace.lock().unwrap().push(format!("flow:{name}"));
            }))
        }
        fn value_flow(&self, _: Syntax<'_>, _: &str) -> ValueFlowFacts {
            panic!("incremental profile requested legacy body summary")
        }
        fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, _: &mut Facts) {
            if ctx.node.kind() == "call" {
                self.0.lock().unwrap().push(format!(
                    "call:{}",
                    ctx.node.utf8_text(ctx.source.as_bytes()).unwrap()
                ));
            }
        }
        fn extract_file(&self, path: &str, source: &str, module: &str, facts: &mut Facts) -> Result<()> {
            super::parse_file(self, path, source, module, facts)
        }
    }

    #[test]
    fn incremental_analysis_finishes_after_its_descendants() {
        let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut facts = Facts::default();
        super::parse_file(
            &Incremental(trace.clone()),
            "probe.py",
            "def outer():\n    first()\n    def inner():\n        nested()\n    last()\n",
            "probe",
            &mut facts,
        )
        .unwrap();
        assert!(facts.errors.is_empty());
        assert_eq!(
            *trace.lock().unwrap(),
            [
                "call:first()",
                "call:nested()",
                "flow:inner",
                "call:last()",
                "flow:outer"
            ]
        );
    }

    #[test]
    fn legacy_hooks_preserve_typed_flow_and_offset_composition() {
        let path = "typed.py";
        let source = "def sample(argument):\n    local = argument\n    return local\n";
        let mut failures = Vec::new();
        for offset in [0, 7] {
            let mut typed =
                TypedFacts::from_public(super::extract(path, "python", "").unwrap());
            if offset == 0 {
                super::parse_file_typed(
                    &Legacy,
                    path,
                    source,
                    "typed",
                    &mut typed,
                    &std::sync::Arc::new(languages::LanguageContext::default()),
                )
                .unwrap();
            } else {
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&Legacy.grammar(path)).unwrap();
                let tree = parser.parse(source, None).unwrap();
                let root = tree.root_node();
                let language_context = languages::LanguageContext::default();
                let rule_ir = language_context.bind_rule_ir(&root.language());
                let summary = SinglePassExtractor {
                    source,
                    path,
                    profile: &Legacy,
                    language_context: &language_context,
                    rule_ir,
                    route_context: routes::RouteContext::build(root, source, &language_context),
                    file: Legacy.prepare(root, source),
                    module: "typed",
                    offset,
                    symbols: HashMap::new(),
                    ids: typed.facts.nodes.iter().map(|n| n.id.clone()).collect(),
                    flows: Some(&mut typed.flows),
                }
                .visit(
                    root,
                    &mut Vec::new(),
                    &format!("module:{path}"),
                    &mut typed.facts,
                );
                if summary.all != ["prepared"] || summary.rebindings != ["rebound"] {
                    failures.push(format!("offset={offset}: exact-root returned summary"));
                }
            }
            let symbol = typed
                .facts
                .nodes
                .iter()
                .find(|n| n.name == "sample")
                .unwrap();
            if symbol.details["is_static"] != true || symbol.line != offset + 1 {
                failures.push(format!("offset={offset}: metadata and owner position"));
            }
            let flow = typed
                .flows
                .get(symbol)
                .expect("actual typed FlowStore entry");
            if flow.return_type
                != Some(TypeExpr::Named {
                    name: "MetadataReturn".into(),
                })
                || flow.alias_type
                    != Some(TypeExpr::Named {
                        name: "Alias".into(),
                    })
                || flow.bindings[0].position.line != offset + 2
            {
                failures.push(format!("offset={offset}: typed flow/return/offset"));
            }
            if !symbol.details["value_flow"].is_null() {
                failures.push(format!("offset={offset}: typed composition must own flow"));
            }
            let module = typed
                .facts
                .nodes
                .iter()
                .find(|n| n.kind == "module")
                .unwrap();
            if module.binding_names().collect::<Vec<_>>() != ["prepared"]
                || module.rebinding_names().collect::<Vec<_>>() != ["rebound"]
            {
                failures.push(format!("offset={offset}: typed root summary publication"));
            }
            let public = serde_json::to_value(&typed).unwrap();
            let symbol = public["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["name"] == "sample")
                .unwrap();
            let decoded: ValueFlowFacts =
                serde_json::from_value(symbol["details"]["value_flow"].clone()).unwrap();
            if decoded.return_type
                != Some(TypeExpr::Named {
                    name: "MetadataReturn".into(),
                })
                || decoded.bindings[0].position.line != offset + 2
            {
                failures.push(format!("offset={offset}: public serialized typed flow"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
