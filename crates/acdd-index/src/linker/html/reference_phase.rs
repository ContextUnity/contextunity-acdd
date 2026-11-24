use crate::languages;
use crate::linker::{
    html,
    resolution::input::ResolutionInput,
    traits::{ReferenceExtension, ReferenceExtensionFactory},
};

pub(crate) struct WorkspaceReferences<'a> {
    templates: html::templates::TemplateRegistry<'a>,
    roots: html::templates::TemplateRootRegistry<'a>,
    scripts: html::scripts::ScriptRegistry<'a>,
}

impl<'a> WorkspaceReferences<'a> {
    pub(crate) fn new(input: &crate::linker::traits::ReferenceWorkspace<'a>) -> Self {
        Self {
            templates: html::templates::TemplateRegistry::build(
                input.all,
                input.flows,
                input.interner,
                input.dependencies,
            ),
            roots: html::templates::TemplateRootRegistry::build(
                input.all,
                input.dependencies,
                input.flows,
                input.interner,
            ),
            scripts: html::scripts::ScriptRegistry::build(
                input.all,
                input.imports,
                input.modules,
                input.dependencies,
            ),
        }
    }
}

impl<'a> ReferenceExtensionFactory<'a> for WorkspaceReferences<'a> {
    fn prepare<'file>(
        &'file self,
        input: &'file ResolutionInput<'a>,
        path: &'file str,
        references: &'file [crate::models::Reference],
        language_context: &'file languages::LanguageContext,
        graph: &mut crate::models::compact_graph::CompactGraph,
    ) -> Option<Box<dyn ReferenceExtension + 'file>>
    where
        'a: 'file,
    {
        let profile = input.profiles.get(path).copied();
        let dependency_registry = input.dependency_registry;
        let by_module = input.by_module;
        let module_nodes = input.module_nodes;
        let html_templates = &self.templates;
        let template_roots = &self.roots;

        let template_loads: Vec<_> = if profile.is_some_and(|profile| profile.id() == "html") {
            references
                .iter()
                .filter(|reference| reference.kind == "template_loads")
                .collect()
        } else {
            Vec::new()
        };
        let template_registry_rules: Vec<_> = if profile
            .is_some_and(|profile| profile.id() == "html")
        {
            language_context
            .syntax_rules
            .iter()
            .filter(|rule| {
                rule.language == "html"
                    && matches!(
                        rule.operation,
                        languages::manifests::syntax::SyntaxOperation::NamedBinding {
                            binding: languages::manifests::projections::NamedBindingForm::TemplateReference { .. },
                            ..
                        }
                    )
            })
            .collect()
        } else {
            Vec::new()
        };
        let template_rules: Vec<_> = if profile.is_some_and(|profile| profile.id() == "html") {
            language_context
                .syntax_rules
                .iter()
                .filter(|rule| {
                    let languages::manifests::syntax::SyntaxOperation::TemplateSymbols {
                        requires_loader,
                        ..
                    } = &rule.operation
                    else {
                        return false;
                    };
                    !*requires_loader
                        || template_roots.admits(path, dependency_registry, &rule.pack)
                })
                .collect()
        } else {
            Vec::new()
        };

        for reference in references.iter().filter(|reference| {
            matches!(
                reference.kind.as_str(),
                "extends" | "includes" | "template_imports"
            )
        }) {
            let target_path = reference.expression.trim();
            let html_target = profile
                .filter(|profile| profile.id() == "html")
                .and_then(|_| {
                    languages::html::local_template_target(path, target_path, language_context)
                });
            let candidate = if let Some(target) = html_target.as_deref() {
                by_module.get(target).and_then(|modules| {
                    modules
                        .iter()
                        .find(|node| node.kind == crate::models::tags::NodeKind::Module)
                        .copied()
                })
            } else if profile.is_some_and(|profile| profile.id() == "html") {
                None
            } else {
                module_nodes
                    .iter()
                    .find(|node| node.path == target_path)
                    .copied()
                    .or_else(|| {
                        let suffix = format!("/{target_path}");
                        module_nodes
                            .iter()
                            .find(|node| node.path.ends_with(&suffix))
                            .copied()
                    })
            };
            if let Some(target) = candidate {
                graph.push_edge(
                    path,
                    reference.source.clone(),
                    target.id.clone(),
                    crate::models::tags::EdgeKind::parse(&reference.kind),
                    reference.line,
                    format!("{path}: {} {}", reference.kind, target.path),
                    crate::models::tags::EdgeConfidence::Exact,
                );
                graph.reference_coverage(
                    path,
                    reference,
                    "resolved",
                    format!("template target: {}", target.path).into(),
                );
            } else {
                graph.reference_coverage(
                    path,
                    reference,
                    "unresolved",
                    format!("template '{}' not found", reference.expression).into(),
                );
            }
        }

        for load in &template_loads {
            if let Some(rule) = template_rules.iter().find(|rule| {
                matches!(
                    &rule.operation,
                    languages::manifests::syntax::SyntaxOperation::TemplateSymbols { libraries, .. }
                        if libraries.iter().any(|library| library == &load.expression)
                )
            }) {
                let label = rule
                    .display
                    .library
                    .as_deref()
                    .unwrap_or("builtin:configured_template");
                graph.reference_coverage(
                    path,
                    load,
                    "external",
                    format!(
                        "{label}; rule:{}:{} {}",
                        rule.pack, rule.id, load.expression
                    )
                    .into(),
                );
                continue;
            }
            match html_templates.library(path, &load.expression, dependency_registry) {
                html::templates::Match::Unique { module, origin, .. } => {
                    let provenance = origin
                        .map(|origin| format!("; rule:{}:{}", origin.pack, origin.rule))
                        .unwrap_or_default();
                    graph.push_edge(
                        path,
                        load.source.clone(),
                        module.id.clone(),
                        crate::models::tags::EdgeKind::Imports,
                        load.line,
                        format!(
                            "template library {} from {}{provenance}",
                            load.expression, module.path
                        ),
                        crate::models::tags::EdgeConfidence::Exact,
                    );
                    graph.reference_coverage(
                        path,
                        load,
                        "resolved",
                        format!("template library: {}{provenance}", module.path).into(),
                    );
                }
                outcome => graph.reference_coverage(
                    path,
                    load,
                    if matches!(outcome, html::templates::Match::Ambiguous) {
                        "ambiguous"
                    } else {
                        "unresolved"
                    },
                    format!(
                        "template library {} has no unique registered provider",
                        load.expression
                    )
                    .into(),
                ),
            }
        }
        Some(Box::new(html::TemplateReferenceState {
            templates: html_templates,
            scripts: &self.scripts,
            loads: template_loads,
            registry_rules: template_registry_rules,
            rules: template_rules,
        }))
    }
}
