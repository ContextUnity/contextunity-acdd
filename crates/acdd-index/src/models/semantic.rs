use super::details::NodeDetailKey;
use serde::{Serialize, Serializer};
use serde_json::Value;

#[derive(Clone, Debug, Default)]
#[doc(hidden)]
pub struct NodeSemanticEntry {
    pub bindings: Vec<String>,
    pub rebindings: Vec<String>,
    pub param_types: Vec<(String, String)>,
    pub(crate) admitted: u8,
}

macro_rules! semantic_fields {
    ($($key:literal => $bit:literal => $field:ident),+ $(,)?) => {
        pub(crate) const SEMANTIC_FIELDS: &[(&'static str, u8)] = &[$(($key, $bit)),+];

        pub fn has(&self, key: &str) -> bool {
            match key { $($key => self.admitted & $bit != 0,)+ _ => false }
        }

        pub fn merge_from(&mut self, incoming: Self) {
            $(if incoming.admitted & $bit != 0 {
                self.$field.extend(incoming.$field);
                self.admitted |= $bit;
            })+
        }
    };
}

pub(crate) fn take_strings(values: Vec<Value>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| match value {
            Value::String(value) => value,
            _ => unreachable!(),
        })
        .collect()
}

pub(crate) fn take_string_pairs(fields: serde_json::Map<String, Value>) -> Vec<(String, String)> {
    fields
        .into_iter()
        .map(|(key, value)| match value {
            Value::String(value) => (key, value),
            _ => unreachable!(),
        })
        .collect()
}

impl NodeSemanticEntry {
    semantic_fields!("bindings" => 1 => bindings, "rebindings" => 2 => rebindings, "param_types" => 4 => param_types);

    pub fn binding_names(&self) -> Option<&[String]> {
        self.has(NodeDetailKey::BINDINGS)
            .then_some(self.bindings.as_slice())
    }

    pub fn rebinding_names(&self) -> Option<&[String]> {
        self.has(NodeDetailKey::REBINDINGS)
            .then_some(self.rebindings.as_slice())
    }

    pub fn parameter_types(&self) -> Option<&[(String, String)]> {
        self.has(NodeDetailKey::PARAM_TYPES)
            .then_some(self.param_types.as_slice())
    }

    pub fn with_parameters(param_types: std::collections::BTreeMap<String, String>) -> Self {
        Self {
            param_types: param_types.into_iter().collect(),
            admitted: 4,
            ..Default::default()
        }
    }

    pub fn parameters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.param_types
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn view(&self) -> impl Serialize + '_ {
        NodeSemanticDto::new(Some(self)).expect("semantic entry has admitted fields")
    }
}

pub struct ParameterTypesView<'a>(pub &'a NodeSemanticEntry);

impl Serialize for ParameterTypesView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.parameters())
    }
}

#[derive(Serialize)]
pub(crate) struct NodeSemanticDto<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bindings: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebindings: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_types: Option<ParameterTypesView<'a>>,
}

impl<'a> NodeSemanticDto<'a> {
    pub fn new(entry: Option<&'a NodeSemanticEntry>) -> Option<Self> {
        let entry = entry?;
        let has_bindings = entry.has("bindings");
        let has_rebindings = entry.has("rebindings");
        let has_param_types = entry.has("param_types");
        if !has_bindings && !has_rebindings && !has_param_types {
            return None;
        }
        Some(Self {
            bindings: has_bindings.then_some(entry.bindings.as_slice()),
            rebindings: has_rebindings.then_some(entry.rebindings.as_slice()),
            param_types: has_param_types.then_some(ParameterTypesView(entry)),
        })
    }

    pub fn filtered_for_base(
        entry: Option<&'a NodeSemanticEntry>,
        base: &serde_json::Map<String, Value>,
    ) -> Option<Self> {
        let mut view = Self::new(entry)?;
        if base.contains_key("bindings") {
            view.bindings = None;
        }
        if base.contains_key("rebindings") {
            view.rebindings = None;
        }
        if base.contains_key("param_types") {
            view.param_types = None;
        }
        (view.bindings.is_some() || view.rebindings.is_some() || view.param_types.is_some())
            .then_some(view)
    }
}
