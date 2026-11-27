use super::details::NodeDetailKey;
use super::semantic::{
    take_string_pairs, take_strings, NodeSemanticDto, NodeSemanticEntry, ParameterTypesView,
};
use super::tags::{LanguageId, NodeKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use std::collections::BTreeMap;

/// Current index semantics version combining the contract epoch and language profile digest.
pub const INDEX_SEMANTICS_VERSION: &str = concat!(
    "16:specs-contract-v1:",
    env!("ACDD_LANGUAGE_PROFILE_DIGEST")
);

#[derive(Debug, Clone, Deserialize)]
/// Indexed source declaration with a stable identity and compact metadata projection.
pub struct Node {
    /// Stable symbol identifier used by graph edges and selectors.
    pub id: String,
    /// Semantic category such as `function`, `method`, or `class`.
    pub kind: NodeKind,
    /// Unqualified declaration name.
    pub name: String,
    /// Fully qualified name within the language namespace.
    pub qualname: String,
    /// Workspace-relative source path.
    pub path: String,
    /// One-based starting source line.
    pub line: usize,
    /// One-based final source line.
    pub end_line: usize,
    /// Whether the declaration belongs to test code.
    pub is_test: bool,
    /// Language profile identifier used for extraction.
    pub language: LanguageId,
    /// Whether the declaration came from generated source.
    pub generated: bool,
    /// Small JSON projection needed for navigation; full AST facts stay separate.
    pub details: Value,
    #[serde(skip)]
    #[doc(hidden)]
    pub native_semantic: Option<Box<NodeSemanticEntry>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
/// Typed semantic metadata for declaration bindings, rebindings, and receiver hints.
pub(crate) struct NodeSemanticMeta {
    /// Resolved bindings for the declaration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<String>,
    /// Resolved rebindings or alias names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rebindings: Vec<String>,
    /// Raw receiver expression (e.g. `(s *Service)` in Go or `self` in Python).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// Resolved receiver type name (e.g. `Service` in Go, struct name in Rust, or prototype in TS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_type: Option<String>,
    /// Optional receiver type hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_hint: Option<String>,
    /// Parameter type annotations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub param_types: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Represents a scanned source or documentation file candidate.
pub struct FileEntry {
    /// Workspace-relative path to the file.
    pub path: String,
    /// Content digest of the file bytes.
    pub digest: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Modification time in nanoseconds.
    pub mtime_ns: u128,
    #[serde(default)]
    /// Device, inode, and mtime identity triple.
    pub identity: [u64; 3],
    /// Canonical language profile ID or detected language.
    pub language: String,
    /// Whether the file is indexed as documentation.
    pub is_doc: bool,
}

static EMPTY_NODE_DETAILS: std::sync::LazyLock<serde_json::Map<String, Value>> =
    std::sync::LazyLock::new(serde_json::Map::new);
static JSON_NULL: Value = Value::Null;

#[derive(Serialize)]
struct NodeDto<'a> {
    id: &'a str,
    kind: &'a str,
    name: &'a str,
    qualname: &'a str,
    path: &'a str,
    line: usize,
    end_line: usize,
    is_test: bool,
    language: &'a str,
    generated: bool,
    #[serde(serialize_with = "serialize_node_only_details")]
    details: &'a Node,
}

#[derive(Serialize)]
struct NodeOnlyDetailsDto<'a> {
    #[serde(flatten)]
    base: &'a serde_json::Map<String, Value>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    semantic: Option<NodeSemanticDto<'a>>,
}

fn serialize_node_only_details<S: serde::Serializer>(
    node: &&Node,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let base = match &node.details {
        Value::Object(base) => base,
        Value::Null => &EMPTY_NODE_DETAILS,
        value => return value.serialize(serializer),
    };
    match NodeSemanticDto::filtered_for_base(node.native_semantic.as_deref(), base) {
        Some(semantic) => NodeOnlyDetailsDto {
            base,
            semantic: Some(semantic),
        }
        .serialize(serializer),
        None if base.is_empty() => serializer.serialize_unit(),
        None => base.serialize(serializer),
    }
}

impl Serialize for Node {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        NodeDto {
            id: &self.id,
            kind: self.kind.as_str(),
            name: &self.name,
            qualname: &self.qualname,
            path: &self.path,
            line: self.line,
            end_line: self.end_line,
            is_test: self.is_test,
            language: self.language.as_str(),
            generated: self.generated,
            details: self,
        }
        .serialize(serializer)
    }
}

impl NodeSemanticEntry {
    pub fn ingest(node: &mut Node) {
        let Some(details) = node.details.as_object_mut() else {
            return;
        };
        for &(key, bit) in Self::SEMANTIC_FIELDS {
            let Some(value) = details.remove(key) else {
                continue;
            };
            if value.is_null()
                && node
                    .native_semantic
                    .as_deref()
                    .is_some_and(|entry| entry.has(key))
            {
                continue;
            }
            let valid = match (key, &value) {
                (NodeDetailKey::PARAM_TYPES, Value::Object(fields)) => {
                    fields.values().all(Value::is_string)
                }
                (NodeDetailKey::BINDINGS | NodeDetailKey::REBINDINGS, Value::Array(values)) => {
                    values.iter().all(Value::is_string)
                }
                _ => false,
            };
            if !valid {
                details.insert(key.to_owned(), value);
                if let Some(entry) = node.native_semantic.as_mut() {
                    entry.admitted &= !bit;
                    match key {
                        NodeDetailKey::BINDINGS => entry.bindings.clear(),
                        NodeDetailKey::REBINDINGS => entry.rebindings.clear(),
                        _ => entry.param_types.clear(),
                    }
                }
                continue;
            }
            let empty = value.as_array().is_some_and(Vec::is_empty)
                || value.as_object().is_some_and(serde_json::Map::is_empty);
            if empty && node.native_semantic.is_none() {
                details.insert(key.to_owned(), value);
                continue;
            }
            let entry = node.native_semantic.get_or_insert_with(Default::default);
            entry.admitted |= bit;
            match (key, value) {
                (NodeDetailKey::PARAM_TYPES, Value::Object(fields)) => {
                    entry.param_types = take_string_pairs(fields)
                }
                (NodeDetailKey::BINDINGS, Value::Array(values)) => {
                    entry.bindings = take_strings(values)
                }
                (NodeDetailKey::REBINDINGS, Value::Array(values)) => {
                    entry.rebindings = take_strings(values)
                }
                _ => unreachable!(),
            }
        }
        if node
            .native_semantic
            .as_deref()
            .is_some_and(|entry| entry.admitted == 0)
        {
            node.native_semantic = None;
        }
    }
}

impl Node {
    /// Assign semantic binding and rebinding sets onto `self`.
    pub fn set_bindings(&mut self, bindings: Vec<String>, rebindings: Vec<String>) {
        NodeSemanticEntry::ingest(self);
        if bindings.is_empty() && rebindings.is_empty() && self.native_semantic.is_none() {
            if let Some(details) = self.details.as_object_mut() {
                details.remove(NodeDetailKey::BINDINGS);
                details.remove(NodeDetailKey::REBINDINGS);
            }
            return;
        }
        let has_bindings = !bindings.is_empty();
        let has_rebindings = !rebindings.is_empty();
        let entry = self.native_semantic.get_or_insert_with(Default::default);
        entry.bindings = bindings;
        entry.rebindings = rebindings;
        entry.admitted |= 3;
        if let Some(details) = self.details.as_object_mut() {
            for (key, present) in [
                (NodeDetailKey::BINDINGS, has_bindings),
                (NodeDetailKey::REBINDINGS, has_rebindings),
            ] {
                if present
                    && details
                        .get(key)
                        .is_some_and(|value| value.as_array().is_some_and(Vec::is_empty))
                {
                    details.remove(key);
                }
            }
        }
    }
    /// Returns a persisted detail entry using its canonical key.
    pub fn detail_entry(&self, key: &str) -> &Value {
        self.details.get(key).unwrap_or(&JSON_NULL)
    }

    /// Returns a detail if present, preserving the distinction between missing and null.
    pub fn detail_get(&self, key: &str) -> Option<&Value> {
        self.details.get(key)
    }

    /// Returns a mutable detail only when it is already present.
    pub fn detail_get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.details.get_mut(key)
    }

    /// Returns a mutable persisted detail entry using its canonical key.
    pub fn detail_entry_mut(&mut self, key: &str) -> &mut Value {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        self.details
            .as_object_mut()
            .expect("node details must be a JSON object")
            .entry(key.to_owned())
            .or_insert(Value::Null)
    }

    /// Stores a detail entry using its canonical key.
    pub fn set_detail(&mut self, key: &'static str, value: Value) {
        if !self.details.is_object() {
            self.details = Value::Object(serde_json::Map::new());
        }
        let details = self
            .details
            .as_object_mut()
            .expect("node details must be a JSON object");
        details.insert(key.to_owned(), value);
    }

    /// Removes a detail entry using its canonical key.
    pub fn remove_detail(&mut self, key: &'static str) {
        if let Some(details) = self.details.as_object_mut() {
            details.shift_remove(key);
        }
    }

    /// Returns a string detail when the stored value has the expected shape.
    pub fn detail_str(&self, key: &'static str) -> Option<&str> {
        self.details.get(key).and_then(Value::as_str)
    }

    /// Returns a boolean detail when the stored value has the expected shape.
    pub fn detail_bool(&self, key: &'static str) -> Option<bool> {
        self.details.get(key).and_then(Value::as_bool)
    }

    /// Returns an unsigned integer detail when the stored value has the expected shape.
    pub fn detail_u64(&self, key: &'static str) -> Option<u64> {
        self.details.get(key).and_then(Value::as_u64)
    }

    fn semantic_entry(&self, key: &str) -> Option<&NodeSemanticEntry> {
        self.native_semantic
            .as_deref()
            .filter(|entry| entry.has(key))
    }

    pub fn has_binding_names(&self) -> bool {
        self.semantic_entry(NodeDetailKey::BINDINGS).is_some()
            || self.detail_entry(NodeDetailKey::BINDINGS).is_array()
    }
    pub fn binding_count(&self) -> usize {
        self.semantic_entry(NodeDetailKey::BINDINGS).map_or_else(
            || {
                self.detail_entry(NodeDetailKey::BINDINGS)
                    .as_array()
                    .map_or(0, Vec::len)
            },
            |entry| entry.binding_names().map_or(0, <[String]>::len),
        )
    }
    pub fn rebinding_count(&self) -> usize {
        self.semantic_entry(NodeDetailKey::REBINDINGS).map_or_else(
            || {
                self.detail_entry(NodeDetailKey::REBINDINGS)
                    .as_array()
                    .map_or(0, Vec::len)
            },
            |entry| entry.rebinding_names().map_or(0, <[String]>::len),
        )
    }
    pub fn parameter_count(&self) -> usize {
        self.semantic_entry(NodeDetailKey::PARAM_TYPES).map_or_else(
            || {
                self.detail_entry(NodeDetailKey::PARAM_TYPES)
                    .as_object()
                    .map_or(0, serde_json::Map::len)
            },
            |entry| entry.parameter_types().map_or(0, <[(String, String)]>::len),
        )
    }
    pub fn has_parameter(&self, name: &str) -> bool {
        self.semantic_entry(NodeDetailKey::PARAM_TYPES).map_or_else(
            || {
                self.detail_entry(NodeDetailKey::PARAM_TYPES)
                    .get(name)
                    .is_some()
            },
            |entry| entry.param_types.iter().any(|(key, _)| key == name),
        )
    }
    pub fn binding_names(&self) -> impl Iterator<Item = &str> {
        self.semantic_entry(NodeDetailKey::BINDINGS)
            .into_iter()
            .flat_map(|entry| {
                entry
                    .binding_names()
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
            })
            .chain(
                self.detail_entry(NodeDetailKey::BINDINGS)
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
    }
    pub fn rebinding_names(&self) -> impl Iterator<Item = &str> {
        self.semantic_entry(NodeDetailKey::REBINDINGS)
            .into_iter()
            .flat_map(|entry| {
                entry
                    .rebinding_names()
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
            })
            .chain(
                self.detail_entry(NodeDetailKey::REBINDINGS)
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
    }
    pub fn parameter_types(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.semantic_entry(NodeDetailKey::PARAM_TYPES)
            .into_iter()
            .flat_map(|entry| {
                entry
                    .parameter_types()
                    .into_iter()
                    .flatten()
                    .map(|(name, ty)| (name.as_str(), Some(ty.as_str())))
            })
            .chain(
                self.detail_entry(NodeDetailKey::PARAM_TYPES)
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, ty)| (name.as_str(), ty.as_str())),
            )
    }
    pub fn parameter_type(&self, name: &str) -> Option<&str> {
        if let Some(entry) = self.semantic_entry(NodeDetailKey::PARAM_TYPES) {
            entry
                .parameters()
                .find_map(|(key, ty)| (key == name).then_some(ty))
        } else {
            self.detail_entry(NodeDetailKey::PARAM_TYPES)
                .get(name)
                .and_then(Value::as_str)
        }
    }
    pub fn parameter_types_value(&self) -> Value {
        if let Some(entry) = self.semantic_entry(NodeDetailKey::PARAM_TYPES) {
            serde_json::to_value(ParameterTypesView(entry)).expect("typed parameters serialize")
        } else {
            self.detail_entry(NodeDetailKey::PARAM_TYPES).clone()
        }
    }
}

impl Node {
    pub(crate) fn navigation_details_with_documentation<'a>(
        &'a self,
        documentation: &'a Value,
    ) -> NavigationDetails<'a> {
        let map = documentation.as_object();
        self.navigation_details(
            map.and_then(|map| map.get(NodeDetailKey::DOC)),
            map.and_then(|map| map.get(NodeDetailKey::DOCSTRING)),
            map.is_some(),
        )
    }

    pub(crate) fn navigation_details_with_doc_fields<'a>(
        &'a self,
        doc: Option<&'a Value>,
        docstring: Option<&'a Value>,
    ) -> NavigationDetails<'a> {
        self.navigation_details(doc, docstring, doc.is_some() || docstring.is_some())
    }

    fn navigation_details<'a>(
        &'a self,
        doc: Option<&'a Value>,
        docstring: Option<&'a Value>,
        has_doc_object: bool,
    ) -> NavigationDetails<'a> {
        NavigationDetails {
            node: self,
            doc,
            docstring,
            has_doc_object,
            skip_inline_docs: true,
            return_type: None,
            entrypoint: None,
            persisted: false,
            clear_entrypoint: false,
        }
    }

    #[cfg(test)]
    fn semantic_field<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.details
            .get(key)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    /// Extracts typed semantic metadata from node details.
    #[cfg(test)]
    pub(crate) fn semantic_meta(&self) -> NodeSemanticMeta {
        let bindings = self
            .semantic_entry(NodeDetailKey::BINDINGS)
            .map(|entry| entry.bindings.clone())
            .unwrap_or_else(|| self.semantic_field(NodeDetailKey::BINDINGS));
        let rebindings = self
            .semantic_entry(NodeDetailKey::REBINDINGS)
            .map(|entry| entry.rebindings.clone())
            .unwrap_or_else(|| self.semantic_field(NodeDetailKey::REBINDINGS));
        let receiver = self
            .details
            .get(NodeDetailKey::RECEIVER)
            .and_then(|v| v.as_str().map(str::to_owned));
        let receiver_type = self
            .details
            .get(NodeDetailKey::RECEIVER_TYPE)
            .and_then(|v| v.as_str().map(str::to_owned));
        let receiver_hint = self
            .details
            .get(NodeDetailKey::RECEIVER_HINT)
            .and_then(|v| v.as_str().map(str::to_owned));
        let param_types = self
            .semantic_entry(NodeDetailKey::PARAM_TYPES)
            .map(|entry| entry.param_types.iter().cloned().collect())
            .unwrap_or_else(|| self.semantic_field(NodeDetailKey::PARAM_TYPES));
        NodeSemanticMeta {
            bindings,
            rebindings,
            receiver,
            receiver_type,
            receiver_hint,
            param_types,
        }
    }

    /// Applies typed semantic metadata into node details.
    #[cfg(test)]
    pub(crate) fn apply_semantic_meta(&mut self, meta: NodeSemanticMeta) {
        self.native_semantic = None;
        if !self.details.is_object() {
            self.details = serde_json::json!({});
        }
        if let Some(details) = self.details.as_object_mut() {
            for (key, value) in [
                (
                    NodeDetailKey::BINDINGS,
                    (!meta.bindings.is_empty()).then(|| serde_json::json!(meta.bindings)),
                ),
                (
                    NodeDetailKey::REBINDINGS,
                    (!meta.rebindings.is_empty()).then(|| serde_json::json!(meta.rebindings)),
                ),
                (
                    NodeDetailKey::PARAM_TYPES,
                    (!meta.param_types.is_empty()).then(|| serde_json::json!(meta.param_types)),
                ),
                (NodeDetailKey::RECEIVER, meta.receiver.map(Value::String)),
                (
                    NodeDetailKey::RECEIVER_TYPE,
                    meta.receiver_type.map(Value::String),
                ),
                (
                    NodeDetailKey::RECEIVER_HINT,
                    meta.receiver_hint.map(Value::String),
                ),
            ] {
                if let Some(value) = value {
                    details.insert(key.into(), value);
                } else {
                    details.remove(key);
                }
            }
        }
    }
}

pub(crate) struct NavigationDetails<'a> {
    node: &'a Node,
    doc: Option<&'a Value>,
    docstring: Option<&'a Value>,
    has_doc_object: bool,
    skip_inline_docs: bool,
    return_type: Option<&'a str>,
    entrypoint: Option<&'a str>,
    persisted: bool,
    clear_entrypoint: bool,
}

impl<'a> NavigationDetails<'a> {
    pub fn persisted(
        mut self,
        return_type: Option<&'a str>,
        entrypoint: Option<&'a str>,
        clear_entrypoint: bool,
    ) -> Self {
        self.return_type = return_type;
        self.entrypoint = entrypoint;
        self.persisted = true;
        self.clear_entrypoint = clear_entrypoint;
        self
    }
}

impl NavigationDetails<'_> {
    fn serialize_details_entries<M: serde::ser::SerializeMap>(
        &self,
        details: &serde_json::Map<String, Value>,
        map: &mut M,
    ) -> Result<(bool, bool), M::Error> {
        let mut return_written = false;
        let mut entrypoint_written = false;
        for (key, value) in details {
            if (self.persisted && key == NodeDetailKey::ENTRYPOINT_BASE)
                || (self.clear_entrypoint && key == NodeDetailKey::ENTRYPOINT)
                || (self.skip_inline_docs
                    && matches!(key.as_str(), NodeDetailKey::DOC | NodeDetailKey::DOCSTRING))
                || (!matches!(
                    key.as_str(),
                    NodeDetailKey::BINDINGS
                        | NodeDetailKey::REBINDINGS
                        | NodeDetailKey::PARAM_TYPES
                ) && !include_navigation_detail(key, value))
            {
                continue;
            }
            if key == NodeDetailKey::RETURN_TYPE {
                if let Some(return_type) = self.return_type {
                    map.serialize_entry(key, return_type)?;
                    return_written = true;
                    continue;
                }
            }
            if key == NodeDetailKey::ENTRYPOINT {
                if let Some(entrypoint) = self.entrypoint {
                    map.serialize_entry(key, entrypoint)?;
                    entrypoint_written = true;
                    continue;
                }
            }
            map.serialize_entry(key, value)?;
        }
        Ok((return_written, entrypoint_written))
    }
}

impl Serialize for NavigationDetails<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let details = self.node.details.as_object();
        let semantic = self.node.native_semantic.as_deref();
        if details.is_none() && !self.has_doc_object && semantic.is_none() {
            if let Some(entrypoint) = self.entrypoint {
                if !self.node.details.is_null() || self.return_type.is_none() {
                    let original = serde_json::to_string(&self.node.details)
                        .map_err(serde::ser::Error::custom)?;
                    let mut map = serializer.serialize_map(Some(2))?;
                    map.serialize_entry(NodeDetailKey::ENTRYPOINT_ORIGINAL_JSON, &original)?;
                    map.serialize_entry(NodeDetailKey::ENTRYPOINT, entrypoint)?;
                    return map.end();
                }
            } else if self.return_type.is_none() || !self.node.details.is_null() {
                return self.node.details.serialize(serializer);
            }
        }
        let mut map = serializer.serialize_map(None)?;
        let (return_written, entrypoint_written) = if let Some(details) = details {
            self.serialize_details_entries(details, &mut map)?
        } else {
            (false, false)
        };
        if let Some(semantic) = semantic {
            let has_detail = |key: &str| details.is_some_and(|details| details.contains_key(key));
            if semantic.has(NodeDetailKey::BINDINGS) && !has_detail(NodeDetailKey::BINDINGS) {
                map.serialize_entry(NodeDetailKey::BINDINGS, &semantic.bindings)?;
            }
            if semantic.has(NodeDetailKey::REBINDINGS) && !has_detail(NodeDetailKey::REBINDINGS) {
                map.serialize_entry(NodeDetailKey::REBINDINGS, &semantic.rebindings)?;
            }
            if semantic.has(NodeDetailKey::PARAM_TYPES) && !has_detail(NodeDetailKey::PARAM_TYPES) {
                map.serialize_entry(NodeDetailKey::PARAM_TYPES, &ParameterTypesView(semantic))?;
            }
        }
        for (key, value) in [
            (NodeDetailKey::DOC, self.doc),
            (NodeDetailKey::DOCSTRING, self.docstring),
        ] {
            if let Some(value) = value.filter(|value| include_navigation_detail(key, value)) {
                map.serialize_entry(key, value)?;
            }
        }
        if let Some(return_type) = self.return_type.filter(|_| !return_written) {
            map.serialize_entry(NodeDetailKey::RETURN_TYPE, return_type)?;
        }
        if let Some(entrypoint) = self.entrypoint.filter(|_| !entrypoint_written) {
            map.serialize_entry(NodeDetailKey::ENTRYPOINT, entrypoint)?;
        }
        map.end()
    }
}

fn include_navigation_detail(key: &str, value: &Value) -> bool {
    !matches!(
        key,
        NodeDetailKey::VALUE_FLOW
            | NodeDetailKey::BINDINGS
            | NodeDetailKey::REBINDINGS
            | NodeDetailKey::PARAM_TYPES
            | NodeDetailKey::COLUMN
            | NodeDetailKey::SEMANTIC_FACTS
            | NodeDetailKey::NAMED_DEFINITION_UNCERTAINTIES
            | NodeDetailKey::NAMED_DEFINITION_UNCERTAINTY_FAULT
    ) && !value.is_null()
        && !value.as_str().is_some_and(str::is_empty)
        && !value.as_array().is_some_and(Vec::is_empty)
        && !(matches!(key, NodeDetailKey::ASYNC | NodeDetailKey::DEFAULT_EXPORT)
            && value == &Value::Bool(false))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Directed relationship between two indexed declarations.
pub struct Edge {
    /// Stable identifier of the source declaration.
    pub src: String,
    /// Stable identifier of the target declaration.
    pub dst: String,
    /// Relationship category, such as a call or import.
    pub kind: String,
    /// Workspace-relative path containing the occurrence.
    pub path: String,
    /// One-based source line of the occurrence.
    pub line: usize,
    /// Evidence expression captured at the occurrence.
    pub evidence: String,
    /// Resolution confidence or status.
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Syntax evidence that helps resolve a reference or its receiver.
pub enum ReceiverHint {
    /// A typed component field admitted by a source-proven macro rule.
    ConfiguredTypedMember {
        /// Actual effective operation identity.
        origin: crate::semantic::ExtractionRuleOrigin,
        /// Exact indexed field identity.
        field_id: String,
    },
    /// A callable result admitted by an existing component macro role.
    ConfiguredMacroCallable {
        /// Actual effective operation identity.
        origin: crate::semantic::ExtractionRuleOrigin,
        /// Lexical producer whose shadowing invalidates this evidence.
        producer: String,
    },
    /// A reference whose interpretation was admitted by a finite extraction rule.
    ConfiguredReference {
        /// Actual effective pack and operation identity.
        origin: crate::semantic::ExtractionRuleOrigin,
    },
    /// A named route key admitted by an effective extraction rule.
    NamedRoute {
        /// Authoritative originating manifest and rule.
        origin: crate::semantic::ExtractionRuleOrigin,
        /// Literal route key, independent of display expression spelling.
        key: String,
    },
    /// Member access on a string literal receiver.
    StringLiteral {
        /// Accessed member name.
        member: String,
    },
    /// Member access on the result of a call.
    CallResult {
        /// Called expression that supplies the receiver.
        callee: String,
        /// Accessed member name.
        member: String,
    },
    /// Member access on a bounded, canonical typed call result.
    TypedCallResult {
        /// Canonical call expression and its original source evidence.
        #[serde(deserialize_with = "crate::semantic::deserialize_typed_call")]
        value: crate::semantic::ValueExpr,
        /// Accessed member.
        member: String,
    },
    /// Member access on an object constructed by a call.
    ConstructorResult {
        /// Constructor expression that supplies the receiver.
        callee: String,
        /// Accessed member name.
        member: String,
    },
    /// Member access through the superclass receiver.
    Super {
        /// Accessed member name.
        member: String,
    },
    /// Vue template identifier declared by the same file's script setup.
    VueSetupBinding,
    /// Custom Vue template element, distinct from a JavaScript expression.
    VueComponentTag,
    /// Vue template member proven by a same-file typed `defineProps` declaration.
    VueTypedPropMember {
        /// Stable ID of the indexed type-literal property.
        field_id: String,
    },
    /// Call through a proven same-file `defineEmits` setup binding.
    VueSetupMacroCallable,
    /// Legacy runtime-callable hint; current runtime admission requires an effective rule.
    VueSetupRuntimeCallable,
    /// TypeScript import binding available only in a type position.
    TypeOnlyImport,
    /// Exact synchronous classic script source declared by an HTML script tag.
    HtmlClassicScriptSource,
    /// Rust call, struct-field, or array-element argument checked for lexical shadowing.
    CallableArg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Unlinked source reference awaiting cross-file resolution.
pub struct Reference {
    /// Stable identifier of the containing declaration.
    pub source: String,
    /// Referenced source expression.
    pub expression: String,
    /// Reference category used by the linker.
    pub kind: String,
    /// One-based source line.
    pub line: usize,
    #[serde(default)]
    /// Byte column within the source line.
    pub column: usize,
    /// Import alias when the source binds another name.
    pub alias: Option<String>,
    /// Imported module path when known.
    pub module: Option<String>,
    #[serde(default)]
    /// Whether resolution depends on runtime behavior.
    pub dynamic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Syntax-derived receiver evidence, if available.
    pub receiver_hint: Option<ReceiverHint>,
}

impl Reference {
    pub fn position(&self) -> crate::semantic::SourcePosition {
        crate::semantic::SourcePosition {
            line: self.line,
            column: self.column,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Source-scoped extraction or linking diagnostic.
pub struct Diagnostic {
    /// Workspace-relative source path.
    pub path: String,
    /// One-based source line.
    pub line: usize,
    /// Human-readable diagnostic message.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Indexed documentation section with extracted invariant and symbol links.
pub struct DocSection {
    /// Stable section identifier.
    pub doc_id: String,
    /// Workspace-relative document path.
    pub path: String,
    /// Heading for this section.
    pub section_title: String,
    /// Document classification.
    pub doc_type: String,
    /// Section body used for search and inspection.
    pub content: String,
    /// Invariant statements extracted from the section.
    pub invariants: Vec<String>,
    /// Symbol references named in the section.
    pub referenced_symbols: Vec<String>,
    /// Source modification time used for freshness checks.
    pub mtime: f64,
    /// Source document size in bytes.
    pub size: u64,
    /// Whether the section expresses an invariant.
    pub is_invariant: bool,
}

/// Normalized AST statement unit extracted inside a callable body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementFingerprint {
    /// Deterministic 64-bit hash of the normalized AST token sequence (excluding comments/trivia).
    pub hash: u64,
    /// One-based starting source line of the statement.
    pub line: usize,
    /// One-based ending source line of the statement.
    pub end_line: usize,
    /// Number of non-trivia AST tokens in the normalized statement.
    pub token_count: usize,
}

/// Ordered AST statement fingerprints belonging to one indexed callable declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallableFingerprints {
    /// Stable symbol ID of the owning callable (`Node.id`).
    pub node_id: String,
    /// Normalized statements in source order.
    pub statements: Vec<StatementFingerprint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Resolution outcome for an import or reference expression.
pub struct Coverage {
    /// Workspace-relative source path.
    pub path: String,
    /// One-based source line.
    pub line: usize,
    /// Expression evaluated by the linker.
    pub expression: String,
    /// Resolution state such as `resolved`, `external`, or `unresolved`.
    pub status: String,
    /// Evidence for the resolution state.
    pub evidence: String,
}

#[derive(Debug, Clone, Default)]
/// Linked graph and resolution coverage ready for persistence.
pub struct Graph {
    /// Resolved relationships.
    pub edges: Vec<Edge>,
    /// Resolution outcomes for source references.
    pub coverage: Vec<Coverage>,
}

/// Computes the deterministic FNV-1a 64-bit hash used for indexed identifiers.
pub use acdd_core::format::stable_hash64;

/// Classifies a workspace-relative path as test code by path conventions.
pub fn is_test(path: &str) -> bool {
    path.split('/').any(|p| p == "tests" || p == "test")
        || path.rsplit('/').next().is_some_and(|p| {
            p.starts_with("test_")
                || p.ends_with("_test.go")
                || p.contains(".test.")
                || p.contains(".spec.")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn node_semantic_meta_roundtrips_with_node_details() {
        let meta = NodeSemanticMeta {
            bindings: vec!["foo".into(), "bar".into()],
            rebindings: vec!["baz".into()],
            receiver: Some("(s *Service)".into()),
            receiver_type: Some("Service".into()),
            receiver_hint: Some("Service".into()),
            param_types: BTreeMap::from([("arg".into(), "str".into())]),
        };
        let serialized = serde_json::to_vec(&meta).unwrap();
        let deserialized: NodeSemanticMeta = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(meta, deserialized);

        let mut node = Node {
            native_semantic: None,
            id: "func:foo".into(),
            path: "src/foo.go".into(),
            language: crate::models::tags::LanguageId::Go,
            kind: crate::models::tags::NodeKind::Method,
            name: "foo".into(),
            qualname: "Service.foo".into(),
            line: 1,
            end_line: 5,
            is_test: false,
            generated: false,
            details: json!({
                "receiver": "(s *Service)",
                "receiver_type": "Service"
            }),
        };
        node.apply_semantic_meta(meta.clone());
        assert_eq!(node.semantic_meta(), meta);

        node.apply_semantic_meta(NodeSemanticMeta::default());
        assert_eq!(node.semantic_meta(), NodeSemanticMeta::default());
        for key in [
            NodeDetailKey::BINDINGS,
            NodeDetailKey::REBINDINGS,
            NodeDetailKey::RECEIVER,
            NodeDetailKey::RECEIVER_TYPE,
            NodeDetailKey::RECEIVER_HINT,
            NodeDetailKey::PARAM_TYPES,
        ] {
            assert!(node.detail_get(key).is_none());
        }
        node.set_detail(NodeDetailKey::VALUE_FLOW, Value::Null);
        assert_eq!(
            node.detail_get(NodeDetailKey::VALUE_FLOW),
            Some(&Value::Null)
        );
    }
}
