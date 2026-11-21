use super::*;
use crate::models::{
    details::NodeDetailKey,
    tags::{EdgeKind, LanguageId, NodeKind},
};

pub(super) fn has_node_binding(
    interner: &crate::interner::LinkerInterner<'_>,
    bindings: &super::super::NodeBindingIndex,
    owner: &str,
    name: &str,
) -> bool {
    let (Some(owner), Some(name)) = (interner.lookup_symbol(owner), interner.lookup_symbol(name))
    else {
        return false;
    };
    bindings
        .get(&owner)
        .is_some_and(|bindings| bindings.binary_search(&name).is_ok())
}

pub(super) struct PhaseContext<'a, 'file, 'res> {
    pub(super) input: &'file ResolutionInput<'a>,
    pub(super) linker: &'file LinkerContext<'a>,
    pub(super) path: &'file str,
    pub(super) facts: &'a Facts,
    pub(super) references: &'file [Reference],
    pub(super) file_module: &'file str,
    pub(super) language_context: &'file languages::LanguageContext,
    pub(super) aliases: &'res HashMap<&'file str, HashMap<&'a str, Vec<&'a Node>>>,
    pub(super) external_aliases: &'res HashMap<&'file str, ExternalAliases<'a>>,
    pub(super) type_only_aliases: &'res HashMap<&'file str, HashSet<&'a str>>,
    #[cfg(feature = "lang-html")]
    pub(super) template_loads: &'res [&'file Reference],
    #[cfg(feature = "lang-html")]
    pub(super) template_registry_rules: &'res [&'file languages::manifests::syntax::SyntaxRule],
    #[cfg(feature = "lang-html")]
    pub(super) template_rules: &'res [&'file languages::manifests::syntax::SyntaxRule],
    pub(super) profile: Option<&'a dyn LanguageProfile>,
    pub(super) language_linker: &'static dyn LanguageLinker,
    pub(super) value_queries: HashSet<(crate::interner::SymbolId, &'a str, usize, usize)>,
    #[cfg(feature = "lang-typescript")]
    pub(super) returned_object_field_writes: HashSet<(&'file str, &'file str)>,
    #[cfg(feature = "lang-typescript")]
    pub(super) lexical_this_calls: HashSet<(&'file str, usize, usize, &'file str)>,
    pub(super) component_module: Option<&'a Node>,
    pub(super) commonjs_module: Option<&'a Node>,
    pub(super) alias_import_positions:
        HashMap<(&'file str, &'file str), crate::semantic::SourcePosition>,
    pub(super) path_constructor_aliases: HashMap<(&'file str, &'file str), &'file Reference>,
    pub(super) dynamic_type_aliases: HashMap<&'file str, HashSet<&'file str>>,
    pub(super) ordinary_member_receivers: HashSet<&'file str>,
}

impl<'a, 'file, 'res> PhaseContext<'a, 'file, 'res>
where
    'a: 'file,
    'file: 'res,
{
    pub(super) fn from_resolver(
        resolver: &'res mut FileResolver<'a, 'file>,
    ) -> (Self, &'res mut Graph) {
        let input = resolver.input;
        let linker = resolver.linker;
        let path = resolver.path;
        let facts = resolver.facts;
        let references = resolver.references;
        let file_module = resolver.file_module;
        let language_context = resolver.language_context;
        let aliases = &resolver.aliases;
        let external_aliases = &resolver.external_aliases;
        let type_only_aliases = &resolver.type_only_aliases;
        #[cfg(feature = "lang-html")]
        let template_loads = resolver.template_loads.as_slice();
        #[cfg(feature = "lang-html")]
        let template_registry_rules = resolver.template_registry_rules.as_slice();
        #[cfg(feature = "lang-html")]
        let template_rules = resolver.template_rules.as_slice();
        let interner = input.interner;
        let flows = input.flows;
        let by_id = input.by_id;
        let profile = input.profiles.get(path).copied();
        let language_linker = profile.map_or(&GENERIC_LINKER as &dyn LanguageLinker, |profile| {
            languages::linker_for(profile.id())
        });
        let value_queries = collect_value_queries(facts, flows, interner);
        #[cfg(feature = "lang-typescript")]
        let returned_object_field_writes =
            super::super::javascript::resolution::returned_object_field_writes(
                profile, references, by_id, interner,
            );
        let component_module = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Module.as_str());
        let commonjs_module = facts.nodes.iter().find(|node| {
            node.kind == NodeKind::Module.as_str()
                && matches!(
                    node.language.as_str(),
                    l if l == LanguageId::Javascript.as_str()
                        || l == LanguageId::Typescript.as_str()
                        || l == LanguageId::Vue.as_str()
                )
        });
        let mut alias_import_positions: HashMap<
            (&'file str, &'file str),
            crate::semantic::SourcePosition,
        > = HashMap::new();
        for reference in references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            if let Some(alias) = reference.alias.as_deref() {
                let scope = interner
                    .lookup_node(by_id, reference.source.as_str())
                    .map(|node| reference_scope(node, path))
                    .unwrap_or("");
                alias_import_positions
                    .entry((scope, alias))
                    .or_insert_with(|| reference.position());
            }
        }
        let path_constructor_aliases = python::resolution::path_constructor_aliases(
            profile,
            references,
            by_id,
            interner,
            file_module,
        );
        let dynamic_type_aliases = python::resolution::dynamic_type_aliases(
            profile,
            references,
            by_id,
            interner,
            file_module,
        );
        #[cfg(feature = "lang-typescript")]
        let lexical_this_calls = super::super::typescript::resolution::lexical_this_calls(
            facts,
            references,
            |node| {
                super::super::typescript::resolution::object_ownership(
                    node,
                    flows,
                    interner,
                    &language_context.syntax_rules,
                )
            },
        );
        let ordinary_member_receivers: HashSet<&'file str> = facts
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Enum.as_str())
            .map(|node| node.name.as_str())
            .chain(
                references
                    .iter()
                    .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
                    .filter_map(|reference| reference.alias.as_deref()),
            )
            .collect();

        (
            Self {
                input,
                linker,
                path,
                facts,
                references,
                file_module,
                language_context,
                aliases,
                external_aliases,
                type_only_aliases,
                #[cfg(feature = "lang-html")]
                template_loads,
                #[cfg(feature = "lang-html")]
                template_registry_rules,
                #[cfg(feature = "lang-html")]
                template_rules,
                profile,
                language_linker,
                value_queries,
                #[cfg(feature = "lang-typescript")]
                returned_object_field_writes,
                #[cfg(feature = "lang-typescript")]
                lexical_this_calls,
                component_module,
                commonjs_module,
                alias_import_positions,
                path_constructor_aliases,
                dynamic_type_aliases,
                ordinary_member_receivers,
            },
            &mut resolver.graph,
        )
    }

    pub(super) fn python_resolution(
        &self,
    ) -> python::resolution::PythonResolutionView<'_, 'res, 'file> {
        python::resolution::PythonResolutionView {
            input: self.input,
            path: self.path,
            file_module: self.file_module,
            aliases: self.aliases,
            alias_import_positions: &self.alias_import_positions,
            member_lookup: &self.linker.member_lookup,
        }
    }

    pub(super) fn lookup(&self, name: &str) -> &[&'a Node] {
        self.input
            .by_qual
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn language_package_exports(&self) -> &'a PackageExports<'a> {
        self.input
            .package_exports
            .get(self.profile.map_or("", |profile| profile.id()))
            .expect("package export index exists for every linked language")
    }

    pub(super) fn reference_profile(&self, reference: &Reference) -> Option<&'a dyn LanguageProfile> {
        #[cfg(feature = "lang-html")]
        {
            html::scripts::reference_profile(
                self.profile,
                reference,
                self.input.by_id,
                self.input.interner,
            )
        }
        #[cfg(not(feature = "lang-html"))]
        {
            let _ = reference;
            self.profile
        }
    }

    pub(super) fn configured_autoimport(&self, name: &str) -> bool {
        self.input.semantic_context.autoimport(self.path, name)
    }

    #[cfg(feature = "lang-typescript")]
    pub(super) fn object_ownership(
        &self,
        node: &Node,
    ) -> Option<&'a crate::semantic::ObjectOwnershipFact> {
        super::super::typescript::resolution::object_ownership(
            node,
            self.input.flows,
            self.input.interner,
            &self.language_context.syntax_rules,
        )
    }

    pub(super) fn configured_component_macro(
        &self,
        name: &str,
        at: crate::semantic::SourcePosition,
        owner: &str,
    ) -> bool {
        let Some((rule, role)) = self.language_context.component_macro(name) else {
            return false;
        };
        let matches_facts = |owner: &Node| {
            self.input
                .flows
                .and_then(|flows| {
                    self.input
                        .interner
                        .lookup_symbol(owner.id.as_str())
                        .and_then(|id| flows.get(&id).copied())
                })
                .is_some_and(|state| match state {
                    FlowState::Valid { facts, .. } => facts.component_macros().any(|fact| {
                        fact.origin.pack == rule.pack
                            && fact.origin.rule == rule.id
                            && fact.role == role
                            && fact.position <= at
                    }),
                    _ => false,
                })
        };
        self.input
            .interner
            .lookup_node(self.input.by_id, owner)
            .is_some_and(matches_facts)
            || self.component_module.is_some_and(matches_facts)
    }

    pub(super) fn builtin_in_scope(&self, expression: &str, reference: &Reference) -> bool {
        if self.configured_autoimport(expression) {
            return true;
        }
        if self.configured_component_macro(expression, reference.position(), &reference.source) {
            return true;
        }
        let active_profile = self.reference_profile(reference);
        #[cfg(feature = "lang-html")]
        if let Some(is_builtin) = html::templates::builtin_in_scope(
            expression,
            reference,
            active_profile,
            self.template_loads,
            self.template_rules,
        ) {
            return is_builtin;
        }
        active_profile.is_some_and(|profile| {
            profile.builtin(expression)
                || (profile.id() == LanguageId::Python.as_str()
                    && expression
                        .split_once('.')
                        .is_some_and(|(receiver, member)| {
                            profile.builtin_type(receiver)
                                && profile.builtin_member(receiver, member)
                        }))
                || super::super::typescript::resolution::builtin_property_chain(
                    expression,
                    profile,
                    self.path,
                    self.input.semantic_context,
                )
        })
    }

    fn walk_alias_scopes<T>(
        &self,
        owner: Option<&Node>,
        definition_scope: bool,
        skip_class_scope_on_definition: bool,
        mut probe: impl FnMut(&str) -> Option<T>,
    ) -> Option<T> {
        let mut scope = owner.map(|n| reference_scope(n, self.path)).unwrap_or("");
        if definition_scope {
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        loop {
            let class_scope = !self.profile.is_some_and(|p| p.class_scope())
                && (!skip_class_scope_on_definition || !definition_scope)
                && owner.is_some_and(|n| {
                    matches!(
                        n.kind.as_str(),
                        k if k == NodeKind::Function.as_str() || k == NodeKind::Method.as_str()
                    )
                })
                && self.input.by_qual.get(scope).is_some_and(|nodes| {
                    nodes.iter().any(|n| {
                        n.path == *self.path
                            && matches!(
                                n.kind.as_str(),
                                k if k == NodeKind::Class.as_str() || k == NodeKind::Impl.as_str()
                            )
                    })
                });
            if !class_scope {
                if let Some(found) = probe(scope) {
                    return Some(found);
                }
            }
            if scope.is_empty() {
                return None;
            }
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
    }

    pub(super) fn alias_for(
        &self,
        name: &str,
        owner: Option<&Node>,
        definition_scope: bool,
    ) -> Option<&'res Vec<&'a Node>> {
        self.walk_alias_scopes(owner, definition_scope, true, |scope| {
            self.aliases.get(scope).and_then(|a| a.get(name))
        })
    }

    pub(super) fn external_for(
        &self,
        name: &str,
        owner: Option<&Node>,
        definition_scope: bool,
    ) -> Option<&'res (String, usize)> {
        self.walk_alias_scopes(owner, definition_scope, false, |scope| {
            self.aliases
                .get(scope)
                .is_some_and(|entries| entries.contains_key(name))
                .then(|| {
                    self.external_aliases
                        .get(scope)
                        .and_then(|entries| entries.get(name))
                        .and_then(Option::as_ref)
                })
                .flatten()
        })
    }

    pub(super) fn commonjs_external_admitted(
        &self,
        name: &str,
        owner: Option<&Node>,
        at: crate::semantic::SourcePosition,
    ) -> bool {
        let Some(module) = self.commonjs_module else {
            return true;
        };
        !self
            .input
            .semantic_context
            .tracks_commonjs_binding(module, name)
            || owner.is_some_and(|owner| {
                self.input
                    .semantic_context
                    .admits_commonjs_binding(module, name, owner, at)
            })
    }

    pub(super) fn external_call_for(
        &self,
        name: &str,
        owner: Option<&Node>,
        at: crate::semantic::SourcePosition,
    ) -> Option<&'res (String, usize)> {
        self.commonjs_external_admitted(name, owner, at)
            .then(|| self.external_for(name, owner, false))
            .flatten()
    }

    pub(super) fn python_nullable_parameter_wrapper(
        &self,
        raw_annotation: &str,
        node: &Node,
    ) -> bool {
        python::resolution::nullable_parameter_wrapper(
            self.profile,
            raw_annotation,
            node,
            self.references,
            |head, node| {
                self.external_for(head, Some(node), true)
                    .map(|(_, line)| *line)
            },
        )
    }

    pub(super) fn is_type_only(
        &self,
        name: &str,
        owner: Option<&Node>,
        definition_scope: bool,
    ) -> bool {
        self.walk_alias_scopes(owner, definition_scope, false, |scope| {
            self.type_only_aliases
                .get(scope)
                .is_some_and(|entries| entries.contains(name))
                .then_some(())
        })
        .is_some()
    }

    pub(super) fn local_receiver_class(&self, owner: &Node) -> Option<&'a Node> {
        owner
            .qualname
            .rsplit_once('.')
            .and_then(|(scope, _)| self.input.by_qual.get(scope))
            .and_then(|nodes| {
                let mut classes = nodes.iter().copied().filter(|node| {
                    node.path == *self.path
                        && matches!(
                            node.kind.as_str(),
                            k if k == NodeKind::Class.as_str()
                                || k == NodeKind::Interface.as_str()
                        )
                });
                let class = classes.next()?;
                classes.next().is_none().then_some(class)
            })
    }

    pub(super) fn is_default_export_vue_module(node: &Node) -> bool {
        node.kind == NodeKind::Module.as_str()
            && node.language == LanguageId::Vue.as_str()
            && node.details[NodeDetailKey::DEFAULT_EXPORT] == true
    }
}

fn collect_value_queries<'a>(
    facts: &Facts,
    flows: Option<&'a HashMap<crate::interner::SymbolId, &'a FlowState>>,
    interner: &crate::interner::LinkerInterner<'_>,
) -> HashSet<(crate::interner::SymbolId, &'a str, usize, usize)> {
    let mut value_queries = HashSet::new();
    for node in &facts.nodes {
        if let Some(FlowState::Valid { facts, .. }) = flows.and_then(|flows| {
            interner
                .lookup_symbol(node.id.as_str())
                .and_then(|id| flows.get(&id).copied())
        }) {
            if !facts.fact_limit_exceeded {
                for fact in facts
                    .annotation_references()
                    .filter(|fact| fact.value_query)
                {
                    value_queries.insert((
                        interner.lookup_symbol(&node.id).expect("catalog owner"),
                        fact.expression.as_str(),
                        fact.position.line,
                        fact.position.column,
                    ));
                }
            }
        }
    }
    value_queries
}
