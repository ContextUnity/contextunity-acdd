use super::flow_store::{FlowState, FlowStore};
use super::semantic_store::{SemanticState, SemanticStateView};
use crate::models::semantic::NodeSemanticDto;
use crate::models::types::Node;
use crate::semantic::types::{NamedDefinitionUncertainty, ValueFlowFacts};
use serde::{Serialize, Serializer};
use serde_json::Value;

macro_rules! serde_dto {
    ($vis:vis $name:ident<$($lifetime:lifetime),+> { $($(#[$attr:meta])* $field:ident: $ty:ty),+ $(,)? }) => {
        #[derive(Serialize)]
        $vis struct $name<$($lifetime),+> { $($(#[$attr])* $field: $ty,)+ }
    };
}

#[derive(Serialize)]
#[serde(untagged)]
enum FlowStateView<'a> {
    Raw(&'a Value),
    Facts(&'a ValueFlowFacts),
}

impl<'a> From<&'a FlowState> for FlowStateView<'a> {
    fn from(state: &'a FlowState) -> Self {
        match state {
            FlowState::Invalid(value) => Self::Raw(value),
            FlowState::Valid { facts } => Self::Facts(facts),
        }
    }
}

impl FlowState {
    pub fn view(&self) -> impl Serialize + '_ {
        FlowStateView::from(self)
    }
}

serde_dto!(pub NodeView<'node, 'flow> {
    id: &'node str, kind: &'node str, name: &'node str, qualname: &'node str,
    path: &'node str, line: usize, end_line: usize, is_test: bool,
    language: &'node str, generated: bool,
    #[serde(serialize_with = "serialize_node_details")] details: NodeDetailsInput<'node, 'flow>,
});

struct NodeDetailsInput<'node, 'flow> {
    node: &'node Node,
    flows: &'flow FlowStore,
    semantic: Option<&'node SemanticState>,
}

impl<'node, 'flow> NodeView<'node, 'flow> {
    pub fn new(
        node: &'node Node,
        flows: &'flow FlowStore,
        semantic: Option<&'node SemanticState>,
    ) -> Self {
        Self {
            id: &node.id,
            kind: node.kind.as_str(),
            name: &node.name,
            qualname: &node.qualname,
            path: &node.path,
            line: node.line,
            end_line: node.end_line,
            is_test: node.is_test,
            language: node.language.as_str(),
            generated: node.generated,
            details: NodeDetailsInput {
                node,
                flows,
                semantic,
            },
        }
    }
}

fn project_if_absent<T>(
    base: &serde_json::Map<String, Value>,
    key: &str,
    project: impl FnOnce() -> Option<T>,
) -> Option<T> {
    if base.contains_key(key) {
        None
    } else {
        project()
    }
}

serde_dto!(NodeDetailsDto<'node, 'flow> {
    #[serde(flatten)] base: &'node serde_json::Map<String, Value>, #[serde(flatten, skip_serializing_if = "Option::is_none")] semantic: Option<NodeSemanticDto<'node>>,
    #[serde(skip_serializing_if = "Option::is_none")] value_flow: Option<FlowStateView<'flow>>, #[serde(skip_serializing_if = "Option::is_none")] semantic_facts: Option<SemanticStateView<'node>>,
    #[serde(skip_serializing_if = "Option::is_none")] named_definition_uncertainties: Option<&'flow [NamedDefinitionUncertainty]>,
    #[serde(skip_serializing_if = "Option::is_none")] named_definition_uncertainty_fault: Option<bool>,
});

static EMPTY_BASE_DETAILS: std::sync::LazyLock<serde_json::Map<String, Value>> =
    std::sync::LazyLock::new(serde_json::Map::new);

impl<'node, 'flow> NodeDetailsDto<'node, 'flow> {
    fn new(
        node: &'node Node,
        base: &'node serde_json::Map<String, Value>,
        flows: &'flow FlowStore,
        semantic_facts: Option<&'node SemanticState>,
    ) -> Option<Self> {
        let semantic = NodeSemanticDto::filtered_for_base(node.native_semantic.as_deref(), base);
        let value_flow = project_if_absent(base, "value_flow", || {
            flows.state(node).map(FlowStateView::from)
        });
        let semantic_facts = project_if_absent(base, "semantic_facts", || {
            semantic_facts.map(SemanticStateView::from)
        });
        let uncertainties = project_if_absent(base, "named_definition_uncertainties", || {
            let markers = flows.named_uncertainties(&node.id);
            (!markers.is_empty()).then_some(markers)
        });
        let uncertainty_fault =
            project_if_absent(base, "named_definition_uncertainty_fault", || {
                flows.named_uncertainty_fault(&node.id).then_some(true)
            });
        let has_projection = semantic.is_some()
            || value_flow.is_some()
            || semantic_facts.is_some()
            || uncertainties.is_some()
            || uncertainty_fault.is_some();
        if !has_projection {
            return None;
        }
        Some(Self {
            base,
            semantic,
            value_flow,
            semantic_facts,
            named_definition_uncertainties: uncertainties,
            named_definition_uncertainty_fault: uncertainty_fault,
        })
    }
}

fn serialize_node_details<S: Serializer>(
    input: &NodeDetailsInput<'_, '_>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let base = match &input.node.details {
        Value::Object(base) => base,
        Value::Null => &EMPTY_BASE_DETAILS,
        value => return value.serialize(serializer),
    };
    match NodeDetailsDto::new(input.node, base, input.flows, input.semantic) {
        Some(projected) => projected.serialize(serializer),
        None if base.is_empty() => serializer.serialize_unit(),
        None => base.serialize(serializer),
    }
}
