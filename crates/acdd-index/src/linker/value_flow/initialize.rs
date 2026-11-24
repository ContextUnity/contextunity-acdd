use super::types::{
    duplicate_position, flow_dependent, merge, node_position, resolve_type, same_fields,
    same_target, stored_type_expr, symbol_type, valid_scope_facts, valid_value, Binding,
    BindingEvent,
    BindingList, Definitions, Epochs, InitializedScope, NamedNodes, ReadEpochs, ReadTrace, Scope,
    ScopeFacts, SemanticLimits, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
    EMPTY_SCOPE_FACTS,
};
use crate::interner::{LinkerInterner, SymbolId};
use crate::models::{Facts, Node, ReceiverHint, Reference};
use crate::semantic::{SourcePosition, ValueExpr, ValueFlowFacts};
use hashbrown::HashMap;
use rayon::prelude::*;
use std::{borrow::Cow, collections::BTreeMap};

fn initialize_scope<'a>(
    node: &'a Node,
    by_name: &NamedNodes<'a>,
    resolver: &impl SemanticResolver<'a>,
    limits: SemanticLimits,
) -> InitializedScope<'a> {
    let encoded = &node.details["value_flow"];
    let typed = resolver.value_flow(node);
    let oversized = typed.is_some_and(|facts| {
        crate::typed_facts::scope_fact_count(facts) > limits.facts_per_scope
    }) || crate::typed_facts::encoded_scope_fact_count(encoded)
        > limits.facts_per_scope;
    let decoded = if oversized {
        Ok(ScopeFacts::Borrowed(&EMPTY_SCOPE_FACTS))
    } else if let Some(facts) = typed {
        Ok(ScopeFacts::Borrowed(facts))
    } else if resolver.has_typed_flows() || encoded.is_null() || oversized {
        Ok(ScopeFacts::Borrowed(&EMPTY_SCOPE_FACTS))
    } else {
        ValueFlowFacts::deserialize_for_scope(
            encoded,
            node.kind == "template_scope"
                && node.language == "javascript"
                && node.details["classic_global"] == true,
        )
        .map(|facts| ScopeFacts::Decoded(Box::new(facts)))
    };
    let (facts, mut invalid) = match decoded {
        Ok(facts) => (facts, false),
        Err(_) => (ScopeFacts::Borrowed(&EMPTY_SCOPE_FACTS), true),
    };
    invalid |=
        oversized || resolver.invalid_value_flow(node) || !valid_scope_facts(node, &facts, limits);
    let mut parents = Vec::new();
    let mut parent = node
        .qualname
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    while !parent.is_empty() {
        if let Some([owner]) = by_name
            .get(&(node.path.as_str(), parent))
            .map(Vec::as_slice)
        {
            if !(node.language == "python"
                && matches!(node.kind.as_str(), "function" | "method")
                && owner.kind == "class")
            {
                parents.push(owner.id.as_str());
            }
        }
        parent = parent.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
    invalid |= parents.len() > limits.alias_depth;
    let mut bindings = HashMap::new();
    if matches!(
        node.kind.as_str(),
        "function" | "method" | "lambda" | "template_scope"
    ) {
        for name in node.binding_names() {
            if name.len() <= limits.name_bytes {
                bindings.insert(
                    name,
                    BindingList::One(Binding {
                        position: node_position(node),
                        target: TypeTarget::Unknown,
                        valid_until: None,
                        restores_previous: false,
                        immutable_initializer: false,
                    }),
                );
            }
        }
    }
    for (name, annotation) in node.parameter_types() {
        let position = node_position(node);
        let target = if invalid || name.len() > limits.name_bytes {
            TypeTarget::Unknown
        } else {
            annotation
                .filter(|name| !name.is_empty() && name.len() <= limits.name_bytes)
                .map_or(TypeTarget::Unknown, |name| {
                    resolve_type(
                        node,
                        &stored_type_expr(name, &node.language),
                        position,
                        resolver,
                        limits,
                    )
                })
        };
        bindings.insert(
            name,
            BindingList::One(Binding {
                position,
                target,
                valid_until: None,
                restores_previous: false,
                immutable_initializer: false,
            }),
        );
    }
    let returned = if invalid {
        TypeTarget::Unknown
    } else if node.language == "rust"
        && node.kind == "constant"
        && node.details["rust_enum_variant"] == true
    {
        node.qualname
            .rsplit_once('.')
            .and_then(|(enum_qual, _)| by_name.get(&(node.path.as_str(), enum_qual)))
            .and_then(|nodes| {
                let mut enums = nodes.iter().copied().filter(|cand| cand.kind == "enum");
                let first = enums.next()?;
                enums.next().is_none().then_some(TypeTarget::Local(first))
            })
            .unwrap_or(TypeTarget::Unknown)
    } else {
        let ty = if node.kind == "constant" {
            facts.alias_type.as_ref()
        } else {
            facts.return_type.as_ref()
        };
        ty.map_or(TypeTarget::Unknown, |ty| {
            resolve_type(node, ty, node_position(node), resolver, limits)
        })
    };
    (
        node,
        facts,
        Scope {
            parents,
            bindings,
            invalid,
            module: node.kind == "module",
        },
        returned,
    )
}

fn field_owner<'a>(
    owner: &'a Node,
    names: &NamedNodes<'a>,
    resolver: &impl SemanticResolver<'a>,
) -> Option<&'a Node> {
    if matches!(owner.kind.as_str(), "class" | "struct" | "interface") {
        return Some(owner);
    }
    if super::super::vue::value_flow::owns_module_fields(owner) {
        return Some(owner);
    }
    let mut parent = owner
        .qualname
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    while !parent.is_empty() {
        if let Some([node]) = names.get(&(owner.path.as_str(), parent)).map(Vec::as_slice) {
            if matches!(node.kind.as_str(), "class" | "struct" | "interface") {
                return Some(*node);
            }
        }
        parent = parent.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
    owner.details["receiver_type"].as_str().and_then(|name| {
        let clean = crate::linker::types::receiver_type_name(name).unwrap_or(Cow::Borrowed(name));
        match resolver.resolve_symbol(
            owner,
            clean.as_ref(),
            node_position(owner),
            SymbolRole::Type,
        ) {
            Symbol::Type(TypeTarget::Local(node)) => Some(node),
            _ => None,
        }
    })
}

impl<'a> ValueFlowIndex<'a> {
    /// Builds value-flow summaries across `all` using default semantic limits.
    pub fn build(all: &'a BTreeMap<String, Facts>, resolver: &impl SemanticResolver<'a>) -> Self {
        Self::build_with_limits(all, resolver, SemanticLimits::default())
    }

    /// Builds value-flow summaries across `all` with caller-specified `limits`.
    pub fn build_with_limits(
        all: &'a BTreeMap<String, Facts>,
        resolver: &impl SemanticResolver<'a>,
        limits: SemanticLimits,
    ) -> Self {
        let nodes: Vec<&Node> = all.values().flat_map(|facts| &facts.nodes).collect();
        let interner = LinkerInterner::from_catalog(all);
        let by_id: HashMap<SymbolId, &Node> = nodes
            .iter()
            .map(|node| {
                (
                    interner
                        .lookup_symbol(&node.id)
                        .expect("catalog node id is interned"),
                    *node,
                )
            })
            .collect();
        Self::build_initialized(
            all,
            nodes,
            &interner,
            &by_id,
            resolver,
            limits,
            |nodes, names| {
                nodes
                    .iter()
                    .map(|&node| initialize_scope(node, names, resolver, limits))
                    .collect()
            },
        )
    }

    pub(crate) fn build_parallel(
        all: &'a dyn crate::FactsLookup,
        nodes: Vec<&'a Node>,
        interner: &LinkerInterner<'a>,
        by_id: &HashMap<SymbolId, &'a Node>,
        resolver: &(impl SemanticResolver<'a> + Sync),
    ) -> Self {
        let limits = SemanticLimits::default();
        Self::build_initialized(
            all,
            nodes,
            interner,
            by_id,
            resolver,
            limits,
            |nodes, names| {
                if nodes.len() >= 8192 {
                    nodes
                        .par_iter()
                        .map(|&node| initialize_scope(node, names, resolver, limits))
                        .collect()
                } else {
                    nodes
                        .iter()
                        .map(|&node| initialize_scope(node, names, resolver, limits))
                        .collect()
                }
            },
        )
    }
    pub(super) fn build_initialized(
        all: &'a dyn crate::FactsLookup,
        nodes: Vec<&'a Node>,
        interner: &crate::interner::LinkerInterner<'a>,
        by_id: &HashMap<crate::interner::SymbolId, &'a Node>,
        resolver: &impl SemanticResolver<'a>,
        limits: SemanticLimits,
        initialize: impl FnOnce(&[&'a Node], &NamedNodes<'a>) -> Vec<InitializedScope<'a>>,
    ) -> Self {
        let by_name = index_nodes_by_name(&nodes);
        let mut index = Self::new_empty(nodes.len(), limits);
        let mut raw = HashMap::with_capacity(nodes.len());
        let initialized = initialize(&nodes, &by_name);
        for (node, facts, scope, returned) in initialized {
            index.record_scope_metadata(node, &facts, &scope, resolver, limits);
            index.returns.insert(node.id.as_str(), returned);
            index.local_types.insert(node.id.as_str(), TypeTarget::Local(node));
            index.scopes.insert(node.id.as_str(), scope);
            raw.insert(node.id.as_str(), facts);
        }
        let owners = sort_owners(nodes);
        let definitions = build_definitions(&owners);
        index.evaluate_slot_macros(&owners, &raw, resolver, &definitions);
        let ordered = index.evaluate_scope_bindings(&owners, &raw, resolver, &definitions);
        let mut field_events =
            index.evaluate_field_facts(&owners, &raw, &by_name, resolver, &definitions);
        super::super::rust::value_flow::project_deref_fields(&mut index.fields, &owners, resolver);
        index.replay_initial_bindings(&owners, &ordered, resolver, &definitions);
        index.run_fixed_point_iteration(
            &owners,
            &raw,
            &ordered,
            &mut field_events,
            resolver,
            &definitions,
            limits,
        );
        index.compute_call_result_hints(all, interner, by_id, resolver, limits);
        index
    }

    fn new_empty(capacity: usize, limits: SemanticLimits) -> Self {
        Self {
            scopes: HashMap::with_capacity(capacity),
            returns: HashMap::with_capacity(capacity),
            async_returns: HashMap::new(),
            return_collection_elements: HashMap::new(),
            async_return_collection_elements: HashMap::new(),
            fields: HashMap::new(),
            component_slots: HashMap::new(),
            local_types: HashMap::with_capacity(capacity),
            computed: HashMap::new(),
            collection_elements: HashMap::new(),
            unknown: TypeTarget::Unknown,
            ambiguous: TypeTarget::Ambiguous,
            limits,
        }
    }

    fn record_scope_metadata(
        &mut self,
        node: &'a Node,
        facts: &ValueFlowFacts,
        scope: &Scope<'a>,
        resolver: &impl SemanticResolver<'a>,
        limits: SemanticLimits,
    ) {
        if scope.invalid {
            return;
        }
        let supports_async = match node.language.as_str() {
            "typescript" | "javascript" | "vue" => {
                matches!(node.kind.as_str(), "function" | "method" | "lambda")
            }
            "python" => matches!(node.kind.as_str(), "function" | "method"),
            _ => false,
        };
        if supports_async {
            if let Some(async_type) = facts.async_return_type.as_ref() {
                let returned = resolve_type(node, async_type, node_position(node), resolver, limits);
                if !matches!(returned, TypeTarget::Unknown | TypeTarget::Ambiguous) {
                    self.async_returns.insert(node.id.as_str(), returned);
                }
                if node.language == "python" {
                    if let Some(element) =
                        super::super::python::value_flow::collection_return_element(
                            node, async_type, resolver, limits,
                        )
                    {
                        self.async_return_collection_elements
                            .insert(node.id.as_str(), element);
                    }
                }
            }
        }
        if node.language != "python" {
            return;
        }
        for fact in facts.collection_elements() {
            let element = resolve_type(node, &fact.element_type, fact.position, resolver, limits);
            if matches!(element, TypeTarget::Unknown | TypeTarget::Ambiguous) {
                continue;
            }
            self.collection_elements
                .entry(node.id.as_str())
                .or_default()
                .entry(fact.name.clone())
                .or_default()
                .push((fact.position, element));
        }
        if matches!(node.kind.as_str(), "function" | "method") {
            if let Some(return_type) = facts.return_type.as_ref() {
                if let Some(element) =
                    super::super::python::value_flow::collection_return_element(
                        node,
                        return_type,
                        resolver,
                        limits,
                    )
                {
                    self.return_collection_elements
                        .insert(node.id.as_str(), element);
                }
            }
        }
    }

    fn evaluate_slot_macros(
        &mut self,
        owners: &[&'a Node],
        raw: &HashMap<&'a str, ScopeFacts<'a>>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) {
        use hashbrown::hash_map::Entry;
        for owner in owners {
            let facts = &raw[owner.id.as_str()];
            if owner.kind != "module"
                || !facts
                    .component_macros()
                    .any(|fact| fact.role == crate::semantic::ComponentMacroRole::Slots)
            {
                continue;
            }
            for slot in facts.vue_slots() {
                let target = if self.scopes[owner.id.as_str()].invalid {
                    TypeTarget::Unknown
                } else {
                    self.evaluate(
                        owner,
                        &ValueExpr::Annotated {
                            type_expr: slot.type_expr.clone(),
                        },
                        slot.position,
                        resolver,
                        Some(definitions),
                    )
                };
                match self
                    .component_slots
                    .entry(owner.id.as_str())
                    .or_default()
                    .entry((slot.slot.clone(), slot.binding.clone()))
                {
                    Entry::Vacant(entry) => {
                        entry.insert(target);
                    }
                    Entry::Occupied(mut entry) => {
                        entry.insert(TypeTarget::Ambiguous);
                    }
                }
            }
        }
    }

    fn evaluate_scope_bindings<'raw>(
        &mut self,
        owners: &[&'a Node],
        raw: &'raw HashMap<&'a str, ScopeFacts<'a>>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) -> HashMap<&'a str, Vec<(BindingEvent<'raw, 'a>, usize)>> {
        use hashbrown::hash_map::Entry;
        let mut ordered = HashMap::with_capacity(owners.len());
        for owner in owners {
            let facts = &raw[owner.id.as_str()];
            let mut bindings = collect_binding_events(owner, facts, resolver.value_flow(owner));
            bindings.sort_by_key(|binding| binding.position);
            let mut events = Vec::with_capacity(bindings.len());
            for binding in bindings {
                if matches!(binding.value, ValueExpr::CallbackParam { .. })
                    && self
                        .scopes
                        .get(owner.id.as_str())
                        .and_then(|scope| scope.bindings.get(binding.name))
                        .is_some_and(|list| {
                            list.iter()
                                .any(|existing| !matches!(existing.target, TypeTarget::Unknown))
                        })
                {
                    continue;
                }
                let target = self.evaluate_single_binding(owner, binding, resolver, definitions);
                self.record_python_collection_return(owner, binding, &target, resolver);
                let write = Binding {
                    position: binding.position,
                    target,
                    valid_until: match binding.value {
                        ValueExpr::LoopElement { body_end, .. }
                        | ValueExpr::Scoped { body_end, .. } => Some(*body_end),
                        _ => None,
                    },
                    restores_previous: owner.language != "python"
                        && matches!(binding.value, ValueExpr::Scoped { .. }),
                    immutable_initializer: binding.immutable_initializer,
                };
                let event_index = match self
                    .scopes
                    .get_mut(owner.id.as_str())
                    .expect("scope indexed")
                    .bindings
                    .entry(binding.name)
                {
                    Entry::Vacant(entry) => {
                        entry.insert(BindingList::One(write));
                        0
                    }
                    Entry::Occupied(mut entry) => {
                        let writes = entry.get_mut();
                        if matches!(binding.value, ValueExpr::CallbackParam { .. })
                            && writes.len() == 1
                            && writes[0].position == write.position
                            && matches!(writes[0].target, TypeTarget::Unknown)
                        {
                            writes[0] = write;
                            0
                        } else {
                            let event_index = writes.len();
                            writes.push(write);
                            event_index
                        }
                    }
                };
                events.push((binding, event_index));
            }
            ordered.insert(owner.id.as_str(), events);
        }
        ordered
    }

    fn prior_binding_target(
        &self,
        owner: &'a Node,
        binding: BindingEvent<'_, 'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> (bool, Option<TypeTarget<'a>>) {
        let loop_element = matches!(binding.value, ValueExpr::LoopElement { .. });
        let lexical_scoped = matches!(binding.value, ValueExpr::Scoped { .. });
        let conditional_rebind = binding.conditional && !loop_element && !lexical_scoped;
        let prev_target = conditional_rebind
            .then(|| {
                self.lookup_tracked(owner, binding.name, binding.position, trace)
                    .cloned()
            })
            .flatten()
            .filter(|prev| !matches!(prev, TypeTarget::Unknown));
        (conditional_rebind && prev_target.is_none(), prev_target)
    }

    fn evaluate_single_binding(
        &self,
        owner: &'a Node,
        binding: BindingEvent<'_, 'a>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) -> TypeTarget<'a> {
        let (missing_prior, prev_target) = self.prior_binding_target(owner, binding, None);
        if self.scopes[owner.id.as_str()].invalid || missing_prior {
            return TypeTarget::Unknown;
        }
        let evaluated = self.evaluate(
            owner,
            binding.value,
            binding.position,
            resolver,
            Some(definitions),
        );
        finalize_binding_target(prev_target, evaluated)
    }

    fn record_python_collection_return(
        &mut self,
        owner: &'a Node,
        binding: BindingEvent<'_, 'a>,
        target: &TypeTarget<'a>,
        resolver: &impl SemanticResolver<'a>,
    ) {
        if owner.language != "python"
            || !matches!(
                target,
                TypeTarget::Builtin(name)
                    if matches!(name.as_str(), "list" | "List" | "Sequence" | "Iterable")
            )
        {
            return;
        }
        let call = match binding.value {
            ValueExpr::Call { callee, .. } => Some((callee.as_str(), false)),
            ValueExpr::Await { value } => match value.as_ref() {
                ValueExpr::Call { callee, .. } => Some((callee.as_str(), true)),
                _ => None,
            },
            _ => None,
        };
        let Some((callee, awaited)) = call else {
            return;
        };
        let callable = self.callable_expression_tracked(
            owner,
            callee,
            binding.position,
            resolver,
            None,
        );
        let return_elements = if awaited {
            &self.async_return_collection_elements
        } else {
            &self.return_collection_elements
        };
        let already_recorded = self
            .collection_elements
            .get(owner.id.as_str())
            .and_then(|names| names.get(binding.name))
            .is_some_and(|elements| {
                elements
                    .iter()
                    .any(|(position, _)| *position == binding.position)
            });
        if already_recorded {
            return;
        }
        if let Some(element) = callable
            .and_then(|callable| return_elements.get(callable.id.as_str()))
            .cloned()
        {
            self.collection_elements
                .entry(owner.id.as_str())
                .or_default()
                .entry(binding.name.to_owned())
                .or_default()
                .push((binding.position, element));
        }
    }

    fn evaluate_field_facts<'raw>(
        &mut self,
        owners: &[&'a Node],
        raw: &'raw HashMap<&'a str, ScopeFacts<'a>>,
        by_name: &NamedNodes<'a>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) -> Vec<(&'a Node, &'a Node, &'raw crate::semantic::FieldFact, TypeTarget<'a>)> {
        let mut field_events = Vec::new();
        for owner in owners {
            if raw[owner.id.as_str()].fields.is_empty() {
                continue;
            }
            let Some(class) = field_owner(owner, by_name, resolver) else {
                continue;
            };
            for field in &raw[owner.id.as_str()].fields {
                let target = if field.conditional || self.scopes[owner.id.as_str()].invalid {
                    TypeTarget::Unknown
                } else {
                    self.evaluate(
                        owner,
                        &field.value,
                        field.position,
                        resolver,
                        Some(definitions),
                    )
                };
                let entry = self
                    .fields
                    .entry(class.id.as_str())
                    .or_default()
                    .entry(field.name.clone())
                    .or_insert_with(|| target.clone());
                *entry = merge(entry, &target);
                field_events.push((*owner, class, field, target));
            }
        }
        field_events
    }

    fn replay_initial_bindings(
        &mut self,
        owners: &[&'a Node],
        ordered: &HashMap<&'a str, Vec<(BindingEvent<'_, 'a>, usize)>>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) {
        for owner in owners {
            if self.scopes[owner.id.as_str()].invalid {
                continue;
            }
            for (binding, event_index) in &ordered[owner.id.as_str()] {
                let writes = &self.scopes[owner.id.as_str()].bindings[binding.name];
                let (missing_prior, prev_target) = self.prior_binding_target(owner, *binding, None);
                let updated = if duplicate_position(writes, *event_index) || missing_prior {
                    TypeTarget::Unknown
                } else if flow_dependent(binding.value) {
                    let evaluated = self.evaluate(
                        owner,
                        binding.value,
                        binding.position,
                        resolver,
                        Some(definitions),
                    );
                    finalize_binding_target(prev_target, evaluated)
                } else {
                    continue;
                };
                self.scopes
                    .get_mut(owner.id.as_str())
                    .expect("scope indexed")
                    .bindings
                    .get_mut(binding.name)
                    .expect("binding indexed")[*event_index]
                    .target = updated;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fixed_point_iteration(
        &mut self,
        owners: &[&'a Node],
        raw: &HashMap<&'a str, ScopeFacts<'a>>,
        ordered: &HashMap<&'a str, Vec<(BindingEvent<'_, 'a>, usize)>>,
        field_events: &mut [(&'a Node, &'a Node, &crate::semantic::FieldFact, TypeTarget<'a>)],
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
        limits: SemanticLimits,
    ) {
        let inferred = collect_inferred_returns(owners, raw, &self.scopes);
        let replay_events = collect_replay_events(owners, ordered, &self.scopes);
        let mut epochs = Epochs::default();
        let mut inferred_reads: Vec<Option<ReadEpochs<'a>>> =
            (0..inferred.len()).map(|_| None).collect();
        let mut replay_reads: Vec<Option<ReadEpochs<'a>>> =
            (0..replay_events.len()).map(|_| None).collect();
        let mut field_reads: Vec<Option<ReadEpochs<'a>>> =
            (0..field_events.len()).map(|_| None).collect();
        for iteration in 0..limits.alias_depth.min(8) {
            if !self.step_inferred_returns(
                &inferred,
                &mut inferred_reads,
                &mut epochs,
                iteration,
                resolver,
                definitions,
            ) {
                break;
            }
            self.step_replay_bindings(
                &replay_events,
                &mut replay_reads,
                &mut epochs,
                resolver,
                definitions,
            );
            self.step_field_events(
                owners,
                field_events,
                &mut field_reads,
                &mut epochs,
                resolver,
                definitions,
            );
        }
    }

    fn step_inferred_returns(
        &mut self,
        inferred: &[(&'a Node, &ValueExpr, SourcePosition)],
        inferred_reads: &mut [Option<ReadEpochs<'a>>],
        epochs: &mut Epochs<'a>,
        iteration: usize,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) -> bool {
        let mut changed = false;
        for ((owner, value, position), reads) in inferred.iter().zip(inferred_reads) {
            if iteration > 0 && !flow_dependent(value) {
                continue;
            }
            if reads.as_ref().is_some_and(|reads| !reads.dirty(epochs)) {
                continue;
            }
            let target = if flow_dependent(value) {
                let trace = ReadTrace::new(epochs, reads.take().unwrap_or_default());
                let target = self.evaluate_tracked(
                    owner,
                    value,
                    *position,
                    resolver,
                    Some(definitions),
                    Some(&trace),
                );
                *reads = Some(trace.reads.into_inner());
                target
            } else {
                self.evaluate(owner, value, *position, resolver, Some(definitions))
            };
            let previous = self
                .returns
                .get_mut(owner.id.as_str())
                .expect("return indexed");
            if !same_target(previous, &target) {
                *previous = target;
                *epochs.returns.entry(owner.id.as_str()).or_default() += 1;
                changed = true;
            }
        }
        changed
    }

    fn step_replay_bindings(
        &mut self,
        replay_events: &[(&'a Node, BindingEvent<'_, 'a>, usize)],
        replay_reads: &mut [Option<ReadEpochs<'a>>],
        epochs: &mut Epochs<'a>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) {
        for ((owner, binding, event_index), reads) in replay_events.iter().zip(replay_reads) {
            if reads.as_ref().is_some_and(|reads| !reads.dirty(epochs)) {
                continue;
            }
            let writes = &self.scopes[owner.id.as_str()].bindings[binding.name];
            let trace = ReadTrace::new(epochs, reads.take().unwrap_or_default());
            let target = if duplicate_position(writes, *event_index) {
                TypeTarget::Unknown
            } else {
                let (missing_prior, prev_target) =
                    self.prior_binding_target(owner, *binding, Some(&trace));
                if missing_prior {
                    TypeTarget::Unknown
                } else {
                    let evaluated = self.evaluate_tracked(
                        owner,
                        binding.value,
                        binding.position,
                        resolver,
                        Some(definitions),
                        Some(&trace),
                    );
                    finalize_binding_target(prev_target, evaluated)
                }
            };
            *reads = Some(trace.reads.into_inner());
            let previous = &mut self
                .scopes
                .get_mut(owner.id.as_str())
                .expect("scope indexed")
                .bindings
                .get_mut(binding.name)
                .expect("binding indexed")[*event_index]
                .target;
            if !same_target(previous, &target) {
                *previous = target;
                *epochs.scopes.entry(owner.id.as_str()).or_default() += 1;
            }
        }
    }

    fn step_field_events(
        &mut self,
        owners: &[&'a Node],
        field_events: &mut [(&'a Node, &'a Node, &crate::semantic::FieldFact, TypeTarget<'a>)],
        field_reads: &mut [Option<ReadEpochs<'a>>],
        epochs: &mut Epochs<'a>,
        resolver: &impl SemanticResolver<'a>,
        definitions: &Definitions<'a>,
    ) {
        let mut field_targets_changed = false;
        for ((owner, _, field, target), reads) in field_events.iter_mut().zip(field_reads) {
            if !field.conditional
                && !self.scopes[owner.id.as_str()].invalid
                && flow_dependent(&field.value)
                && reads.as_ref().is_none_or(|reads| reads.dirty(epochs))
            {
                let trace = ReadTrace::new(epochs, reads.take().unwrap_or_default());
                let updated = self.evaluate_tracked(
                    owner,
                    &field.value,
                    field.position,
                    resolver,
                    Some(definitions),
                    Some(&trace),
                );
                *reads = Some(trace.reads.into_inner());
                if !same_target(target, &updated) {
                    *target = updated;
                    field_targets_changed = true;
                }
            }
        }
        if !field_targets_changed {
            return;
        }
        let mut fields: HashMap<&str, HashMap<String, TypeTarget<'a>>> = HashMap::new();
        for (_, class, field, target) in field_events.iter() {
            let entry = fields
                .entry(class.id.as_str())
                .or_default()
                .entry(field.name.clone())
                .or_insert_with(|| target.clone());
            *entry = merge(entry, target);
        }
        super::super::rust::value_flow::project_deref_fields(&mut fields, owners, resolver);
        if !same_fields(&self.fields, &fields) {
            epochs.fields += 1;
        }
        self.fields = fields;
    }

    fn compute_call_result_hints(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        interner: &crate::interner::LinkerInterner<'a>,
        by_id: &HashMap<crate::interner::SymbolId, &'a Node>,
        resolver: &impl SemanticResolver<'a>,
        limits: SemanticLimits,
    ) {
        for reference in all.values_facts().flat_map(|facts| &facts.references) {
            if !matches!(
                reference.receiver_hint,
                Some(
                    ReceiverHint::ConstructorResult { .. }
                        | ReceiverHint::CallResult { .. }
                        | ReceiverHint::TypedCallResult { .. }
                )
            ) {
                continue;
            }
            let Some(owner) = interner.lookup_node(by_id, reference.source.as_str()) else {
                continue;
            };
            let at = reference.position();
            let target = self.evaluate_reference_hint(owner, reference, at, resolver, limits);
            let Some(target) = target else {
                continue;
            };
            self.computed
                .entry((
                    owner.id.as_str(),
                    at,
                    reference.expression.as_str(),
                    reference.kind.as_str(),
                ))
                .and_modify(|existing| *existing = merge(existing, &target))
                .or_insert(target);
        }
    }

    fn evaluate_reference_hint(
        &self,
        owner: &'a Node,
        reference: &Reference,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        limits: SemanticLimits,
    ) -> Option<TypeTarget<'a>> {
        let target = match reference.receiver_hint.as_ref() {
            Some(ReceiverHint::TypedCallResult { value, .. }) => {
                if matches!(value, ValueExpr::Call { .. }) && valid_value(value, limits, 0) {
                    self.evaluate(owner, value, at, resolver, None)
                } else {
                    TypeTarget::Unknown
                }
            }
            Some(ReceiverHint::ConstructorResult { callee, .. }) => {
                if callee.len() > limits.name_bytes {
                    TypeTarget::Unknown
                } else {
                    symbol_type(resolver.resolve_symbol(
                        owner,
                        callee,
                        at,
                        SymbolRole::Constructor,
                    ))
                }
            }
            Some(ReceiverHint::CallResult { callee, .. }) => {
                self.evaluate_call_result_hint(owner, callee, at, resolver, limits)
            }
            _ => return None,
        };
        Some(target)
    }

    fn evaluate_call_result_hint(
        &self,
        owner: &'a Node,
        callee: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        limits: SemanticLimits,
    ) -> TypeTarget<'a> {
        if let Some(rest) = callee.strip_prefix("value:") {
            let rest = rest.trim();
            if !(rest.starts_with('{') && rest.ends_with('}')) {
                return TypeTarget::Unknown;
            }
            return serde_json::from_str::<ValueExpr>(rest)
                .ok()
                .filter(|value| valid_value(value, limits, 0))
                .map_or(TypeTarget::Unknown, |value| {
                    self.evaluate(owner, &value, at, resolver, None)
                });
        }
        if callee.len() > limits.name_bytes || callee.matches('.').count() > limits.alias_depth {
            return TypeTarget::Unknown;
        }
        if let Some(inner) = callee
            .strip_suffix("()")
            .filter(|_| owner.language == "python")
        {
            return self
                .type_of_expression(owner, inner, at, resolver)
                .and_then(|target| {
                    super::super::python::value_flow::callable_target_return(
                        self, target, owner, at, resolver, None,
                    )
                })
                .cloned()
                .unwrap_or(TypeTarget::Unknown);
        }
        let (callee_name, is_try) = match callee.strip_suffix('?') {
            Some(base) => (base, true),
            None => (callee, false),
        };
        let call_expr = ValueExpr::Call {
            callee: callee_name.to_owned(),
            site: None,
        };
        let expr = if is_try {
            ValueExpr::Project {
                value: Box::new(call_expr),
                member: "?".to_owned(),
            }
        } else {
            call_expr
        };
        self.evaluate(owner, &expr, at, resolver, None)
    }
}

fn index_nodes_by_name<'a>(nodes: &[&'a Node]) -> NamedNodes<'a> {
    let mut by_name: NamedNodes<'a> = HashMap::with_capacity(nodes.len());
    for node in nodes {
        by_name
            .entry((&node.path, &node.qualname))
            .or_default()
            .push(node);
    }
    by_name
}

fn sort_owners(mut nodes: Vec<&Node>) -> Vec<&Node> {
    nodes.sort_by_cached_key(|node| {
        (
            node.path.as_str(),
            node.qualname.matches('.').count(),
            node.line,
            node.id.as_str(),
        )
    });
    nodes
}

fn build_definitions<'a>(owners: &[&'a Node]) -> Definitions<'a> {
    owners
        .iter()
        .fold(HashMap::with_capacity(owners.len()), |mut map, node| {
            map.entry((node.path.as_str(), node_position(node)))
                .or_default()
                .push(*node);
            map
        })
}

fn collect_binding_events<'a, 'raw>(
    owner: &'a Node,
    facts: &'raw ScopeFacts<'a>,
    typed: Option<&'a ValueFlowFacts>,
) -> Vec<BindingEvent<'raw, 'a>> {
    facts
        .bindings
        .iter()
        .enumerate()
        .map(|(event_index, binding)| BindingEvent {
            name: typed.map_or_else(
                || {
                    owner.details["value_flow"]["bindings"][event_index]["name"]
                        .as_str()
                        .expect("decoded binding name remains in arena")
                },
                |facts| facts.bindings[event_index].name.as_str(),
            ),
            position: binding.position,
            value: &binding.value,
            conditional: binding.conditional,
            immutable_initializer: facts
                .immutable_initializers
                .binary_search(&binding.position)
                .is_ok(),
        })
        .chain(
            facts
                .branch_joins
                .iter()
                .enumerate()
                .map(|(event_index, join)| BindingEvent {
                    name: typed.map_or_else(
                        || {
                            owner.details["value_flow"]["branch_joins"][event_index]["name"]
                                .as_str()
                                .expect("decoded branch-join name remains in arena")
                        },
                        |facts| facts.branch_joins[event_index].name.as_str(),
                    ),
                    position: join.position,
                    value: &join.value,
                    conditional: join.conditional,
                    immutable_initializer: false,
                }),
        )
        .collect()
}

fn collect_inferred_returns<'a, 'raw>(
    owners: &[&'a Node],
    raw: &'raw HashMap<&'a str, ScopeFacts<'a>>,
    scopes: &HashMap<&'a str, Scope<'a>>,
) -> Vec<(&'a Node, &'raw ValueExpr, SourcePosition)> {
    owners
        .iter()
        .filter_map(|owner| {
            let facts = &raw[owner.id.as_str()];
            if facts.return_type.is_some() || scopes[owner.id.as_str()].invalid {
                return None;
            }
            facts
                .return_value
                .as_ref()
                .zip(facts.return_position)
                .map(|(value, position)| (*owner, value, position))
        })
        .collect()
}

fn collect_replay_events<'a, 'raw>(
    owners: &[&'a Node],
    ordered: &HashMap<&'a str, Vec<(BindingEvent<'raw, 'a>, usize)>>,
    scopes: &HashMap<&'a str, Scope<'a>>,
) -> Vec<(&'a Node, BindingEvent<'raw, 'a>, usize)> {
    owners
        .iter()
        .filter(|owner| !scopes[owner.id.as_str()].invalid)
        .flat_map(|owner| {
            let scope_bindings = &scopes[owner.id.as_str()].bindings;
            ordered[owner.id.as_str()]
                .iter()
                .filter(move |(binding, _)| {
                    flow_dependent(binding.value)
                        || (binding.conditional
                            && !matches!(
                                binding.value,
                                ValueExpr::LoopElement { .. } | ValueExpr::Scoped { .. }
                            )
                            && scope_bindings
                                .get(binding.name)
                                .is_some_and(|writes| writes.len() > 1))
                })
                .map(move |(binding, event_index)| (*owner, *binding, *event_index))
        })
        .collect()
}

fn unwrap_rust_ref<'a>(mut target: &'a TypeTarget<'a>) -> &'a TypeTarget<'a> {
    while let TypeTarget::RustReference { target: inner, .. } = target {
        target = inner;
    }
    target
}

fn finalize_binding_target<'a>(
    prev_target: Option<TypeTarget<'a>>,
    evaluated: TypeTarget<'a>,
) -> TypeTarget<'a> {
    match prev_target {
        Some(prev)
            if matches!(evaluated, TypeTarget::Unknown)
                || same_target(unwrap_rust_ref(&prev), unwrap_rust_ref(&evaluated)) =>
        {
            prev
        }
        Some(_) => TypeTarget::Unknown,
        None => evaluated,
    }
}
