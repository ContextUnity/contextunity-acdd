use crate::{
    models::types::Node,
    semantic::{
        builtins::DEFAULT_FACTS_PER_SCOPE,
        types::{encoded_scope_fact_count, scope_fact_count, ValueFlowFacts},
    },
};
use serde_json::Value;
use std::collections::HashMap;

/// A named uncertainty carrier is malformed or explicitly marked faulty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvalidNamedUncertaintyCarrier;

pub(crate) fn remap_identifier(
    value: &mut String,
    ids: &HashMap<String, String>,
) -> bool {
    let Some(replacement) = ids.get(value) else {
        return false;
    };
    value.clone_from(replacement);
    true
}

#[derive(Clone, Debug)]
pub enum FlowState {
    Valid {
        facts: Box<ValueFlowFacts>,
    },
    Invalid(Value),
}

#[derive(Clone, Debug, Default)]
pub struct FlowStore {
    entries: std::collections::BTreeMap<String, FlowState>,
    uncertainties:
        std::collections::BTreeMap<String, Vec<crate::semantic::types::NamedDefinitionUncertainty>>,
    uncertainty_faults: std::collections::BTreeSet<String>,
    invalid_uncertainty_carriers: std::collections::BTreeSet<String>,
}

macro_rules! append_fields {
    ($target:ident, $incoming:ident; $($field:ident),+ $(,)?) => {
        $($target.$field.append(&mut $incoming.$field);)+
    };
}

macro_rules! merge_optional_fields {
    ($target:ident, $incoming:ident; $($field:ident),+ $(,)?) => {
        $($target.fact_limit_exceeded |= merge_optional(
            &mut $target.$field, $incoming.$field,
        );)+
    };
}

fn merge_optional<T: PartialEq>(previous: &mut Option<T>, incoming: Option<T>) -> bool {
    match (&*previous, incoming) {
        (None, value) => {
            *previous = value;
            false
        }
        (Some(previous), Some(value)) => previous != &value,
        _ => false,
    }
}

impl FlowStore {
    pub fn cache_entries(&self) -> impl ExactSizeIterator<Item = (&str, &FlowState)> {
        self.entries
            .iter()
            .map(|(owner, state)| (owner.as_str(), state))
    }
    pub fn restore_cache_entry(
        &mut self,
        owner: String,
        state: Option<FlowState>,
        uncertainties: Vec<crate::semantic::types::NamedDefinitionUncertainty>,
        uncertainty_fault: bool,
    ) {
        if let Some(state) = state {
            self.entries.insert(owner.clone(), state);
        }
        if !uncertainties.is_empty() {
            self.uncertainties.insert(owner.clone(), uncertainties);
        }
        if uncertainty_fault {
            self.uncertainty_faults.insert(owner);
        }
    }
    pub fn named_uncertainties(
        &self,
        owner: &str,
    ) -> &[crate::semantic::types::NamedDefinitionUncertainty] {
        self.uncertainties.get(owner).map_or(&[], Vec::as_slice)
    }
    pub fn named_uncertainty_entries(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &[crate::semantic::types::NamedDefinitionUncertainty])> {
        self.uncertainties
            .iter()
            .map(|(id, values)| (id.as_str(), values.as_slice()))
    }
    pub fn named_uncertainty_faults(&self) -> impl ExactSizeIterator<Item = &str> {
        self.uncertainty_faults.iter().map(String::as_str)
    }
    pub fn named_uncertainty_fault(&self, owner: &str) -> bool {
        self.uncertainty_faults.contains(owner)
    }
    pub fn has_invalid_named_uncertainty_carriers(&self) -> bool {
        !self.invalid_uncertainty_carriers.is_empty()
    }
    pub(crate) fn read_named_uncertainties(
        node: &Node,
    ) -> Result<
        Vec<crate::semantic::types::NamedDefinitionUncertainty>,
        InvalidNamedUncertaintyCarrier,
    > {
        let markers = Self::read_named_uncertainty_carrier(node)
            .map_err(|()| InvalidNamedUncertaintyCarrier)?;
        if node
            .detail_bool(crate::models::details::NodeDetailKey::NAMED_DEFINITION_UNCERTAINTY_FAULT)
            == Some(true)
        {
            Err(InvalidNamedUncertaintyCarrier)
        } else {
            Ok(markers)
        }
    }
    fn read_named_uncertainty_carrier(
        node: &Node,
    ) -> Result<Vec<crate::semantic::types::NamedDefinitionUncertainty>, ()> {
        let fault = node
            .detail_get(crate::models::details::NodeDetailKey::NAMED_DEFINITION_UNCERTAINTY_FAULT);
        if fault.is_some_and(|value| !value.is_boolean()) {
            return Err(());
        }
        let Some(raw) = node
            .detail_get(crate::models::details::NodeDetailKey::NAMED_DEFINITION_UNCERTAINTIES)
        else {
            return Ok(Vec::new());
        };
        let values = raw.as_array().ok_or(())?;
        if values.len() > 128 * 128 {
            return Err(());
        }
        let markers: Vec<crate::semantic::types::NamedDefinitionUncertainty> =
            serde_json::from_value(raw.clone()).map_err(|_| ())?;
        let mut grouped: std::collections::BTreeMap<
            (&str, &str, &str),
            Vec<&crate::semantic::types::StaticString>,
        > = std::collections::BTreeMap::new();
        for marker in &markers {
            if !marker.valid() {
                return Err(());
            }
            let keys = grouped
                .entry((&marker.domain, &marker.origin.pack, &marker.origin.rule))
                .or_default();
            if keys.contains(&&marker.key) {
                return Err(());
            }
            keys.push(&marker.key);
            if keys.len() > 128
                || (keys.len() > 1 && keys.contains(&&crate::semantic::types::StaticString::Unknown))
            {
                return Err(());
            }
        }
        if grouped.len() > 128 {
            return Err(());
        }
        Ok(markers)
    }
    fn merge_uncertainties(
        &mut self,
        id: &str,
        incoming: impl IntoIterator<Item = crate::semantic::types::NamedDefinitionUncertainty>,
    ) {
        use crate::semantic::types::{NamedDefinitionUncertainty, StaticString};
        let mut groups: std::collections::BTreeMap<(String, String, String), Vec<StaticString>> =
            std::collections::BTreeMap::new();
        let previous = self.uncertainties.remove(id).unwrap_or_default();
        for marker in previous.into_iter().chain(incoming) {
            if !marker.valid() {
                self.uncertainty_faults.insert(id.to_owned());
                self.invalid_uncertainty_carriers.insert(id.to_owned());
                return;
            }
            let keys = groups
                .entry((marker.domain, marker.origin.pack, marker.origin.rule))
                .or_default();
            if !keys.contains(&StaticString::Unknown) && !keys.contains(&marker.key) {
                if marker.key == StaticString::Unknown || keys.len() == 128 {
                    keys.clear();
                    keys.push(StaticString::Unknown);
                } else {
                    keys.push(marker.key);
                }
            }
            if groups.len() > 128 {
                self.uncertainty_faults.insert(id.to_owned());
                return;
            }
        }
        let markers: Vec<_> = groups
            .into_iter()
            .flat_map(|((domain, pack, rule), keys)| {
                keys.into_iter().map(move |key| NamedDefinitionUncertainty {
                    domain: domain.clone(),
                    key,
                    origin: crate::semantic::types::ExtractionRuleOrigin {
                        pack: pack.clone(),
                        rule: rule.clone(),
                    },
                })
            })
            .collect();
        if !markers.is_empty() {
            self.uncertainties.insert(id.to_owned(), markers);
        }
    }
    fn capture_uncertainties(&mut self, id: &str, facts: &ValueFlowFacts) {
        let markers = facts
            .named_bindings()
            .filter(|fact| {
                matches!(
                    fact.role,
                    crate::semantic::types::NamedBindingRole::Definition { .. }
                        | crate::semantic::types::NamedBindingRole::RegistryDefinition { .. }
                )
            })
            .map(|fact| crate::semantic::types::NamedDefinitionUncertainty {
                domain: fact.domain.clone(),
                key: fact.key.clone(),
                origin: fact.origin.clone(),
            });
        self.merge_uncertainties(id, markers);
    }
    fn invalid_overflow(mut original: Value) -> FlowState {
        if let Some(facts) = original.as_object_mut() {
            facts.remove("type_only_imports");
            facts.remove("annotation_references");
            facts.insert("fact_limit_exceeded".into(), Value::Bool(true));
        }
        FlowState::Invalid(original)
    }

    pub fn state(&self, node: &Node) -> Option<&FlowState> {
        self.entries.get(&node.id)
    }

    pub fn get(&self, node: &Node) -> Option<&ValueFlowFacts> {
        match self.state(node) {
            Some(FlowState::Valid { facts, .. }) => Some(facts),
            _ => None,
        }
    }

    pub fn raw(&self, node: &Node) -> Option<&Value> {
        match self.state(node) {
            Some(FlowState::Valid { .. }) => None,
            Some(FlowState::Invalid(value)) => Some(value),
            None => None,
        }
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ValueFlowFacts> {
        match self.entries.get_mut(id) {
            Some(FlowState::Valid { facts }) => Some(facts),
            _ => None,
        }
    }

    fn exceeds_fact_limit(facts: &ValueFlowFacts) -> bool {
        scope_fact_count(facts) > DEFAULT_FACTS_PER_SCOPE
            || facts.symbol_attachments().count() > 128
            || facts.factory_results().count() > 128
            || facts.named_bindings().count() > 128
            || facts.endpoint_requests().count() > 128
            || facts.factory_results().any(|fact| fact.members.len() > 128)
            || facts.fact_limit_exceeded
            || facts
                .bindings
                .iter()
                .map(|fact| &fact.value)
                .chain(facts.fields.iter().map(|fact| &fact.value))
                .chain(facts.branch_joins.iter().map(|fact| &fact.value))
                .chain(facts.return_value.iter())
                .any(|value| !value.valid_call_sites(0))
    }

    pub fn insert(&mut self, id: String, facts: ValueFlowFacts) {
        self.insert_boxed(id, Box::new(facts));
    }
    pub fn insert_boxed(&mut self, id: String, facts: Box<ValueFlowFacts>) {
        self.insert_with_mode(id, facts);
    }
    pub fn insert_classic(&mut self, id: String, facts: ValueFlowFacts) {
        self.insert_with_mode(id, Box::new(facts));
    }

    fn insert_with_mode(&mut self, id: String, facts: Box<ValueFlowFacts>) {
        self.uncertainties.remove(&id);
        self.uncertainty_faults.remove(&id);
        self.invalid_uncertainty_carriers.remove(&id);
        if Self::exceeds_fact_limit(&facts) {
            self.capture_uncertainties(&id, &facts);
            let original = serde_json::to_value(facts)
                .expect("value-flow facts contain only JSON-compatible values");
            self.entries.insert(id, Self::invalid_overflow(original));
            return;
        }
        self.entries.insert(id, FlowState::Valid { facts });
    }

    pub fn ingest(&mut self, node: &mut Node) {
        match Self::read_named_uncertainty_carrier(node) {
            Ok(markers) => {
                let has_markers = !markers.is_empty();
                self.merge_uncertainties(&node.id, markers);
                if has_markers {
                    if let Some(details) = node.details.as_object_mut() {
                        details.remove(
                            crate::models::details::NodeDetailKey::NAMED_DEFINITION_UNCERTAINTIES,
                        );
                    }
                }
            }
            Err(()) => {
                self.uncertainty_faults.insert(node.id.clone());
                self.invalid_uncertainty_carriers.insert(node.id.clone());
            }
        }
        if node
            .detail_bool(crate::models::details::NodeDetailKey::NAMED_DEFINITION_UNCERTAINTY_FAULT)
            == Some(true)
        {
            self.uncertainty_faults.insert(node.id.clone());
            node.remove_detail(
                crate::models::details::NodeDetailKey::NAMED_DEFINITION_UNCERTAINTY_FAULT,
            );
        }
        let classic = node.kind == crate::models::tags::NodeKind::TemplateScope
            && node.language == crate::models::tags::LanguageId::Javascript
            && node.detail_bool(crate::models::details::NodeDetailKey::CLASSIC_GLOBAL)
                == Some(true);
        let Some(details) = node.details.as_object_mut() else {
            return;
        };
        let Some(original) = details.remove(crate::models::details::NodeDetailKey::VALUE_FLOW)
        else {
            return;
        };
        if original.is_null() {
            return;
        }
        if encoded_scope_fact_count(&original) > DEFAULT_FACTS_PER_SCOPE
            || original["fact_limit_exceeded"] == true
        {
            self.entries
                .insert(node.id.clone(), Self::invalid_overflow(original));
            return;
        }
        let state = match ValueFlowFacts::deserialize_for_scope(&original, classic) {
            Ok(facts) => FlowState::Valid {
                facts: Box::new(facts),
            },
            Err(_) => FlowState::Invalid(original),
        };
        self.entries.insert(node.id.clone(), state);
    }

    pub fn rename(&mut self, old: &str, new: String) {
        if self.invalid_uncertainty_carriers.remove(old) {
            self.invalid_uncertainty_carriers.insert(new.clone());
        }
        if let Some(markers) = self.uncertainties.remove(old) {
            self.uncertainties.insert(new.clone(), markers);
        }
        if self.uncertainty_faults.remove(old) {
            self.uncertainty_faults.insert(new.clone());
        }
        if let Some(state) = self.entries.remove(old) {
            self.entries.insert(new, state);
        }
    }

    /// Rewrite only explicit canonical operands after an embedded island is named.
    pub fn remap_nodes(&mut self, ids: &HashMap<String, String>) {
        use crate::semantic::types::{NamedBindingRole, SymbolOperand};
        let remap = |operand: &mut SymbolOperand| {
            if let SymbolOperand::Node { id } = operand {
                remap_identifier(id, ids);
            }
        };
        for state in self.entries.values_mut() {
            let FlowState::Valid { facts } = state
            else {
                continue;
            };
            let Some(framework) = facts.framework.as_mut() else {
                continue;
            };
            for fact in framework.symbol_attachments_mut() {
                remap(&mut fact.source);
                remap(&mut fact.target);
            }
            for fact in framework.named_bindings_mut() {
                match &mut fact.role {
                    NamedBindingRole::Definition { target, .. } => remap(target),
                    NamedBindingRole::RegistryDefinition { receiver, target } => {
                        remap(receiver);
                        remap(target);
                    }
                    NamedBindingRole::Reference => {}
                }
            }
        }
    }

    fn merge_uncertainty_state(
        &mut self,
        id: &str,
        invalid_carrier: bool,
        markers: Vec<crate::semantic::types::NamedDefinitionUncertainty>,
        fault: bool,
    ) {
        if invalid_carrier {
            self.invalid_uncertainty_carriers.insert(id.to_owned());
        }
        self.merge_uncertainties(id, markers);
        if fault {
            self.uncertainty_faults.insert(id.to_owned());
        }
    }

    /// Move a complete classic fragment, retaining invalid raw evidence and bounds.
    pub fn merge_classic(&mut self, id: &str, other: &mut Self) {
        let incoming_invalid_carrier = other.invalid_uncertainty_carriers.remove(id);
        let invalid_carrier =
            self.invalid_uncertainty_carriers.contains(id) || incoming_invalid_carrier;
        let incoming_markers = other.uncertainties.remove(id).unwrap_or_default();
        let incoming_fault = other.uncertainty_faults.remove(id);
        let Some(incoming) = other.entries.remove(id) else {
            self.merge_uncertainty_state(id, invalid_carrier, incoming_markers, incoming_fault);
            return;
        };
        let Some(previous) = self.entries.remove(id) else {
            let previous_markers = self.uncertainties.remove(id).unwrap_or_default();
            let previous_fault = self.uncertainty_faults.remove(id);
            match incoming {
                FlowState::Valid { facts, .. } => self.insert_classic(id.to_owned(), *facts),
                FlowState::Invalid(raw) => {
                    self.entries.insert(id.to_owned(), FlowState::Invalid(raw));
                }
            }
            self.merge_uncertainties(id, previous_markers);
            self.merge_uncertainty_state(
                id,
                invalid_carrier,
                incoming_markers,
                incoming_fault || previous_fault,
            );
            return;
        };
        if matches!(&previous, FlowState::Invalid(_)) || matches!(&incoming, FlowState::Invalid(_))
        {
            if let FlowState::Valid { facts, .. } = &previous {
                self.capture_uncertainties(id, facts);
            }
            if let FlowState::Valid { facts, .. } = &incoming {
                self.capture_uncertainties(id, facts);
            }
        }
        let merged = match (previous, incoming) {
            (FlowState::Invalid(raw), _) | (_, FlowState::Invalid(raw)) => FlowState::Invalid(raw),
            (
                FlowState::Valid { mut facts },
                FlowState::Valid {
                    facts: incoming, ..
                },
            ) => {
                let mut incoming = *incoming;
                append_fields!(facts, incoming; bindings, branch_joins, immutable_initializers, fields);
                if let Some(mut incoming_framework) = incoming.framework.take().map(|facts| *facts)
                {
                    if !incoming_framework.is_empty() {
                        let framework = facts.framework.get_or_insert_with(Default::default);
                        framework.fact_limit_exceeded |= incoming_framework.fact_limit_exceeded;
                        merge_optional_fields!(framework, incoming_framework;
                            object_ownership, async_return_type, return_value, return_position, alias_type, callable_signature,
                        );
                        framework.facts.append(&mut incoming_framework.facts);
                    }
                }
                facts.fact_limit_exceeded |=
                    merge_optional(&mut facts.return_type, incoming.return_type);
                if Self::exceeds_fact_limit(&facts) {
                    self.capture_uncertainties(id, &facts);
                    Self::invalid_overflow(
                        serde_json::to_value(facts).expect("serializable classic flow"),
                    )
                } else {
                    FlowState::Valid { facts }
                }
            }
        };
        self.entries.insert(id.to_owned(), merged);
        self.merge_uncertainty_state(id, invalid_carrier, incoming_markers, incoming_fault);
    }

    pub fn append(&mut self, other: &mut Self) {
        append_fields!(self, other; entries, uncertainties, uncertainty_faults, invalid_uncertainty_carriers);
    }

    pub fn retain_nodes(&mut self, ids: &std::collections::HashSet<String>) {
        self.entries.retain(|id, _| ids.contains(id));
        self.uncertainties.retain(|id, _| ids.contains(id));
        self.uncertainty_faults.retain(|id| ids.contains(id));
        self.invalid_uncertainty_carriers
            .retain(|id| ids.contains(id));
    }

    pub fn take(&mut self, id: &str) -> Option<ValueFlowFacts> {
        match self.entries.remove(id) {
            Some(FlowState::Valid { facts, .. }) => Some(*facts),
            _ => None,
        }
    }

    /// Drops linker-only flow data, keeping only persistence-needed return types and template contexts.
    pub fn retain_persistence_facts(&mut self, keep_return_types: bool) {
        self.uncertainties.clear();
        self.uncertainty_faults.clear();
        self.invalid_uncertainty_carriers.clear();
        self.entries.retain(|_, state| {
            let FlowState::Valid { facts } = state
            else {
                return false;
            };
            facts.bindings = Vec::new();
            facts.branch_joins = Vec::new();
            facts.immutable_initializers = Vec::new();
            facts.fields = Vec::new();
            if !keep_return_types {
                facts.return_type = None;
            }
            if let Some(framework) = facts.framework.as_mut() {
                framework.object_ownership = None;
                framework.fact_limit_exceeded = false;
                if !keep_return_types {
                    framework.async_return_type = None;
                }
                framework.return_value = None;
                framework.return_position = None;
                framework.alias_type = None;
                framework.callable_signature = None;
                framework
                    .facts
                    .retain(|fact| matches!(fact, crate::semantic::types::FrameworkFact::TemplateContext(_)));
                if framework.async_return_type.is_none() && framework.facts.is_empty() {
                    facts.framework = None;
                }
            }
            facts.return_type.is_some() || facts.framework.is_some()
        });
    }
}
