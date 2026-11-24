//! Prepared, borrowed literal-key joins for configured language facts.
use crate::interner::{LinkerInterner, PathId, SymbolId};
use crate::models::Node;
use crate::semantic::{
        ExtractionRuleOrigin, NamedBindingFact, NamedBindingRole, NamedDefinitionUncertainty,
        NamedTargetKind, StaticString, SymbolOperand,
    };
use crate::typed_facts::FlowState;
use crate::languages::{self, dependency_registry::DependencyRegistry};
use hashbrown::{HashMap, HashSet};

pub(super) fn project<'a>(path: &'a str, dependencies: &'a DependencyRegistry) -> &'a str {
    crate::languages::dependency_registry::named_binding_project(path, dependencies)
}

pub(super) fn active(
    origin: &ExtractionRuleOrigin,
    path: &str,
    dependencies: &DependencyRegistry,
) -> bool {
    let rules = dependencies.language_context_for_path(path);
    active_in_context(origin, &rules)
}

fn active_in_context(origin: &ExtractionRuleOrigin, rules: &languages::LanguageContext) -> bool {
    rules
        .syntax_rules
        .iter()
        .any(|r| r.pack == origin.pack && r.id == origin.rule)
        || rules
            .semantic_rules
            .iter()
            .any(|r| r.active && r.pack == origin.pack && r.rule.id == origin.rule)
}

pub(super) struct Definition<'a> {
    pub owner: &'a Node,
    pub fact: &'a NamedBindingFact,
}

pub(super) struct RegistryDefinition<'a> {
    pub definition: Definition<'a>,
    pub root: RegistryRoot,
    pub receiver_type: languages::manifests::projections::ImportedReceiverType,
    pub target: Result<Option<&'a Node>, ()>,
}

#[derive(Clone, Copy)]
pub(super) struct RegistryRoot {
    config_path: PathId,
    template_root: SymbolId,
}

type IncompleteRegistry = (RegistryRoot, Option<SymbolId>);

impl RegistryRoot {
    pub(super) fn contains_path(self, path: PathId, interner: &LinkerInterner<'_>) -> bool {
        let config = interner.resolve_path(self.config_path);
        let directory = config
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        let candidate = interner.resolve_path(path);
        let relative = if directory.is_empty() {
            candidate
        } else {
            let Some(relative) = candidate
                .strip_prefix(directory)
                .and_then(|tail| tail.strip_prefix('/'))
            else {
                return false;
            };
            relative
        };
        relative
            .strip_prefix(interner.resolve_symbol(self.template_root))
            .is_some_and(|tail| tail.starts_with('/'))
    }
}

#[derive(Default)]
pub(super) struct NamedBindings<'a> {
    definitions: HashMap<(PathId, SymbolId, SymbolId), Vec<Definition<'a>>>,
    unknown: HashMap<(PathId, SymbolId), Vec<Definition<'a>>>,
    incomplete: HashMap<(PathId, SymbolId), Vec<Option<SymbolId>>>,
    incomplete_registries: HashMap<(PathId, SymbolId), Vec<IncompleteRegistry>>,
    unavailable_scopes: HashSet<PathId>,
    reference_scopes: HashMap<(PathId, SymbolId, SymbolId), Vec<PathId>>,
    pub registries: HashMap<(PathId, SymbolId), Vec<RegistryDefinition<'a>>>,
    pub factories: HashMap<(SymbolId, SymbolId), FactoryDefinition<'a>>,
    pub positions: HashMap<(PathId, crate::semantic::SourcePosition), Vec<&'a Node>>,
}

pub(super) struct FactoryDefinition<'a> {
    pub facts: Vec<&'a crate::semantic::FactoryResultFact>,
    pub value: super::value_flow::TypeTarget<'a>,
}

impl<'a> NamedBindings<'a> {
    fn collect_call_reference_scopes(
        &mut self,
        scope: PathId,
        rules: &languages::LanguageContext,
        interner: &LinkerInterner<'a>,
        dependencies: &'a DependencyRegistry,
    ) {
        use languages::manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        for rule in &rules.syntax_rules {
            let SyntaxOperation::NamedBinding {
                binding: NamedBindingForm::CallReference { provider_scopes, .. },
                ..
            } = &rule.operation
            else {
                continue;
            };
            if provider_scopes.is_empty() {
                continue;
            }
            let mut scopes = vec![scope];
            scopes.extend(provider_scopes.iter().filter_map(|provider| {
                dependencies.named_provider_scope(provider).map(|scope| {
                    interner
                        .lookup_path(scope)
                        .expect("named provider scope is interned")
                })
            }));
            scopes.sort_unstable();
            scopes.dedup();
            let (Some(pack), Some(rule_id)) = (
                interner.lookup_symbol(&rule.pack),
                interner.lookup_symbol(&rule.id),
            ) else {
                continue;
            };
            self.reference_scopes.insert((scope, pack, rule_id), scopes);
        }
    }

    fn record_owner_uncertainties(
        &mut self,
        path: &str,
        scope: PathId,
        markers: &[NamedDefinitionUncertainty],
        rules: &languages::LanguageContext,
        interner: &LinkerInterner<'a>,
    ) {
        use languages::manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        for marker in markers {
            let binding = rules.syntax_rules.iter().find_map(|rule| {
                if rule.pack != marker.origin.pack || rule.id != marker.origin.rule {
                    return None;
                }
                match &rule.operation {
                    SyntaxOperation::NamedBinding { domain, binding }
                        if domain == &marker.domain =>
                    {
                        Some(binding)
                    }
                    _ => None,
                }
            });
            let domain = interner
                .lookup_symbol(&marker.domain)
                .expect("named-binding domain is interned");
            let key = match &marker.key {
                StaticString::Literal { value } => Some(
                    interner
                        .lookup_symbol(value)
                        .expect("named-binding literal key is interned"),
                ),
                StaticString::Unknown => None,
            };
            match binding {
                Some(NamedBindingForm::AssignmentDefinition { template_root, .. }) => {
                    let root = RegistryRoot {
                        config_path: interner
                            .lookup_path(path)
                            .expect("named-binding config path is interned"),
                        template_root: interner
                            .lookup_symbol(template_root)
                            .expect("named-binding template root is interned"),
                    };
                    self.incomplete_registries
                        .entry((scope, domain))
                        .or_default()
                        .push((root, key));
                }
                Some(
                    NamedBindingForm::CallDefinition { .. }
                    | NamedBindingForm::AttributeDefinition { .. },
                ) => {
                    self.incomplete
                        .entry((scope, domain))
                        .or_default()
                        .push(key);
                }
                _ => {}
            }
        }
    }

    fn record_owner_flows(
        &mut self,
        path: &str,
        scope: PathId,
        owner: &'a Node,
        facts: &'a crate::semantic::ValueFlowFacts,
        rules: &languages::LanguageContext,
        interner: &LinkerInterner<'a>,
    ) {
        if facts.fact_limit_exceeded {
            return;
        }
        for fact in facts.factory_results() {
            if !active_in_context(&fact.origin, rules) {
                continue;
            }
            self.factories
                .entry((
                    interner.lookup_symbol(&owner.id).expect("catalog owner"),
                    interner
                        .lookup_symbol(&fact.binding)
                        .expect("factory binding is interned"),
                ))
                .or_insert_with(|| FactoryDefinition {
                    facts: Vec::new(),
                    value: super::value_flow::TypeTarget::Unknown,
                })
                .facts
                .push(fact);
        }
        for fact in facts.named_bindings() {
            if !active_in_context(&fact.origin, rules) {
                continue;
            }
            if matches!(fact.role, NamedBindingRole::RegistryDefinition { .. }) {
                self.record_registry_definition(path, scope, owner, fact, rules, interner);
                continue;
            }
            if !matches!(fact.role, NamedBindingRole::Definition { .. }) {
                continue;
            }
            let definition = Definition { owner, fact };
            let domain = interner
                .lookup_symbol(&fact.domain)
                .expect("named-binding domain is interned");
            match &fact.key {
                StaticString::Literal { value } => {
                    let key = interner
                        .lookup_symbol(value)
                        .expect("named-binding literal key is interned");
                    self.definitions
                        .entry((scope, domain, key))
                        .or_default()
                        .push(definition);
                }
                StaticString::Unknown => {
                    self.unknown
                        .entry((scope, domain))
                        .or_default()
                        .push(definition);
                }
            }
        }
    }

    fn record_registry_definition(
        &mut self,
        path: &str,
        scope: PathId,
        owner: &'a Node,
        fact: &'a crate::semantic::NamedBindingFact,
        rules: &languages::LanguageContext,
        interner: &LinkerInterner<'a>,
    ) {
        use languages::manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        let policy = rules
            .syntax_rules
            .iter()
            .find(|rule| rule.pack == fact.origin.pack && rule.id == fact.origin.rule);
        if let Some(SyntaxOperation::NamedBinding {
            binding:
                NamedBindingForm::AssignmentDefinition {
                    receiver_type,
                    template_root,
                    ..
                },
            ..
        }) = policy.map(|rule| &rule.operation)
        {
            let root = RegistryRoot {
                config_path: interner
                    .lookup_path(path)
                    .expect("named-binding config path is interned"),
                template_root: interner
                    .lookup_symbol(template_root)
                    .expect("named-binding template root is interned"),
            };
            let domain = interner
                .lookup_symbol(&fact.domain)
                .expect("named-binding domain is interned");
            self.registries
                .entry((scope, domain))
                .or_default()
                .push(RegistryDefinition {
                    definition: Definition { owner, fact },
                    root,
                    receiver_type: receiver_type.clone(),
                    target: Ok(None),
                });
        }
    }

    fn resolve_factory_positions(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        interner: &LinkerInterner<'a>,
        nodes: &HashMap<SymbolId, &'a Node>,
    ) {
        let mut position_paths = HashSet::<PathId>::new();
        for ((owner, _), definition) in &self.factories {
            if let [fact] = definition.facts.as_slice() {
                if !fact.members.is_empty() {
                    if let Some(owner) = nodes.get(owner).copied() {
                        position_paths
                            .insert(interner.lookup_path(&owner.path).expect("catalog path"));
                    }
                }
            }
        }
        for definition in self.definitions.values().flatten() {
            let NamedBindingRole::Definition {
                target: SymbolOperand::Node { id },
                target_kind,
            } = &definition.fact.role
            else {
                continue;
            };
            let Some(target_id) = interner.lookup_symbol(id) else {
                continue;
            };
            if let Some(target) = nodes.get(&target_id).copied().filter(|target| {
                target.path == definition.owner.path
                    && match target_kind {
                        NamedTargetKind::Object => target.kind == crate::models::tags::NodeKind::Object,
                        NamedTargetKind::ListenerScope => target.kind == crate::models::tags::NodeKind::TemplateScope,
                        NamedTargetKind::Callable => false,
                    }
            }) {
                position_paths.insert(interner.lookup_path(&target.path).expect("catalog path"));
            }
        }
        if !position_paths.is_empty() {
            // Keep every competing node at a demanded path, including duplicate
            // positions and nodes stored under a different inventory key.
            for owner in all.values_facts().flat_map(|file| &file.nodes) {
                let path = interner.lookup_path(&owner.path).expect("catalog path");
                if position_paths.contains(&path) {
                    self.positions
                        .entry((
                            path,
                            crate::semantic::SourcePosition {
                                line: owner.line,
                                column: owner.detail_entry(crate::models::details::NodeDetailKey::COLUMN).as_u64().unwrap_or(0) as usize,
                            },
                        ))
                        .or_default()
                        .push(owner);
                }
            }
        }
        for ((owner, _), definition) in &mut self.factories {
            use super::value_flow::TypeTarget;
            let [fact] = definition.facts.as_slice() else {
                definition.value = TypeTarget::Ambiguous;
                continue;
            };
            let Some(owner) = nodes.get(owner) else {
                continue;
            };
            let mut members = HashMap::new();
            let mut value = None;
            for member in &fact.members {
                let mut candidates = self
                    .positions
                    .get(&(
                        interner.lookup_path(&owner.path).expect("catalog path"),
                        member.position,
                    ))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| node.name == member.declaration_name());
                let Some(node) = candidates.next() else {
                    value = Some(TypeTarget::Unknown);
                    break;
                };
                if candidates.next().is_some() || members.insert(member.name.clone(), node).is_some()
                {
                    value = Some(TypeTarget::Ambiguous);
                    break;
                }
            }
            definition.value =
                value.unwrap_or_else(|| TypeTarget::Object(std::sync::Arc::new(members)));
        }
    }

    pub(crate) fn build_lookup(
        all: &'a dyn crate::FactsLookup,
        interner: &LinkerInterner<'a>,
        flows: Option<&'a HashMap<SymbolId, &'a FlowState>>,
        uncertainties: Option<&'a HashMap<SymbolId, &'a [NamedDefinitionUncertainty]>>,
        faults: Option<&HashSet<SymbolId>>,
        dependencies: &'a DependencyRegistry,
        nodes: &HashMap<SymbolId, &'a Node>,
    ) -> Self {
        let mut index = Self::default();
        let mut scanned_reference_scopes: HashSet<(PathId, usize)> = HashSet::new();
        let mut pinned_rules: Vec<std::sync::Arc<languages::LanguageContext>> = Vec::new();
        for (path, file) in all.iter_facts() {
            let scope = interner
                .lookup_path(project(path, dependencies))
                .expect("project scope is interned");
            let rules = dependencies.language_context_for_path(path);
            if scanned_reference_scopes.insert((scope, std::sync::Arc::as_ptr(&rules) as usize)) {
                pinned_rules.push(std::sync::Arc::clone(&rules));
                index.collect_call_reference_scopes(scope, &rules, interner, dependencies);
            }
            for owner in &file.nodes {
                let owner_id = interner
                    .lookup_symbol(&owner.id)
                    .expect("named-binding owner id is interned");
                if faults.is_some_and(|faults| faults.contains(&owner_id)) {
                    index.unavailable_scopes.insert(scope);
                }
                if let Some(markers) = uncertainties.and_then(|m| m.get(&owner_id).copied()) {
                    index.record_owner_uncertainties(path, scope, markers, &rules, interner);
                }
                if let Some(FlowState::Valid { facts, .. }) =
                    flows.and_then(|flows| flows.get(&owner_id).copied())
                {
                    index.record_owner_flows(path, scope, owner, facts, &rules, interner);
                }
            }
        }
        index.resolve_factory_positions(all, interner, nodes);
        index
    }

    pub fn definitions<'ctx>(
        &'ctx self,
        scope: PathId,
        domain: SymbolId,
        key: SymbolId,
    ) -> (&'ctx [Definition<'a>], bool) {
        let scope_domain = (scope, domain);
        (
            self.definitions
                .get(&(scope, domain, key))
                .map_or(&[], Vec::as_slice),
            self.unavailable_scopes.contains(&scope)
                || self.unknown.contains_key(&scope_domain)
                || self.incomplete.get(&scope_domain).is_some_and(|keys| {
                    keys.iter()
                        .any(|candidate| candidate.is_none_or(|candidate| candidate == key))
                }),
        )
    }

    pub fn scopes(&self, scope: PathId, pack: SymbolId, rule: SymbolId) -> Option<&[PathId]> {
        self.reference_scopes
            .get(&(scope, pack, rule))
            .map(Vec::as_slice)
    }

    pub fn registry_definitions<'ctx>(
        &'ctx self,
        scope: PathId,
        domain: SymbolId,
    ) -> &'ctx [RegistryDefinition<'a>] {
        self.registries
            .get(&(scope, domain))
            .map_or(&[], Vec::as_slice)
    }

    pub fn registry_uncertain(
        &self,
        scope: PathId,
        domain: SymbolId,
        key: SymbolId,
        path: PathId,
        interner: &LinkerInterner<'_>,
    ) -> bool {
        self.unavailable_scopes.contains(&scope)
            || self
                .incomplete_registries
                .get(&(scope, domain))
                .is_some_and(|markers| {
                    markers.iter().any(|(root, candidate)| {
                        root.contains_path(path, interner)
                            && candidate.is_none_or(|candidate| candidate == key)
                    })
                })
    }
}
