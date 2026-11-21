//! Version-2 `GateProfile` schema definitions, patch types, and strict `deny_unknown_fields` deserialization.

use crate::types::{IsolationMode, SaveAction, WorktreeLifecycle};
use acdd_core::format::{canonical_json_sha256, canonical_json_string};
use acdd_core::subsystems::{SubsystemPreset, SubsystemRegistry};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateId, GateSelector};
use acdd_core::schema::SchemaDef;
use schemars::JsonSchema;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Default maximum gate retry/cycle attempts when omitted on a `GateDef`.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Three-state patch wrapper distinguishing omitted (`Unset`), explicit `null` (`Clear`), and value (`Set`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PatchField<T> {
    /// Key was omitted in YAML; keep inherited value.
    #[default]
    Unset,
    /// Key was explicitly set to `null`; clear inherited value to `None`/empty.
    Clear,
    /// Key was explicitly set to a non-null value.
    Set(T),
}

impl<T> PatchField<T> {
    /// Apply this patch to an `Option<U>` target using `map_fn`.
    pub fn apply_to_option<U>(&self, target: &mut Option<U>, map_fn: impl FnOnce(&T) -> U) {
        match self {
            Self::Unset => {}
            Self::Clear => *target = None,
            Self::Set(val) => *target = Some(map_fn(val)),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for PatchField<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Option::<T>::deserialize(deserializer)? {
            None => Ok(Self::Clear),
            Some(v) => Ok(Self::Set(v)),
        }
    }
}

/// Flexible Git isolation and worktree policy (`git:`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitPolicy {
    /// Workspace isolation strategy (`worktree` or `in_place`).
    pub isolation: IsolationMode,
    /// Worktree lifecycle ownership (`manual` or `managed`).
    pub worktrees: WorktreeLifecycle,
}

impl Default for GitPolicy {
    fn default() -> Self {
        Self {
            isolation: IsolationMode::Worktree,
            worktrees: WorktreeLifecycle::Manual,
        }
    }
}

/// Field-wise patch representation for `git:`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitPolicyPatch {
    /// Override `git.isolation`.
    pub isolation: Option<IsolationMode>,
    /// Override `git.worktrees`.
    pub worktrees: Option<WorktreeLifecycle>,
}

/// Execution mode for a configured role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleExecutionMode {
    /// Run the role as a separate worker/subagent.
    #[default]
    Subagent,
    /// Run the role inline in the active session.
    Inline,
}

impl fmt::Display for RoleExecutionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Subagent => "subagent",
            Self::Inline => "inline",
        })
    }
}

/// Model recommendation entry within a `RoleDef` (`"model"`, `"model:reasoning"`, or `{ model, reasoning? }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ModelSpec {
    /// Model identifier.
    pub model: String,
    /// Optional reasoning effort level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reasoning {
            Some(reasoning) => write!(f, "{}:{reasoning}", self.model),
            None => f.write_str(&self.model),
        }
    }
}

/// Compact coordinator-facing dispatch metadata for a gate (`acdd start` / `acdd next`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateDispatchInfo {
    /// Runtime node cursor (`NextResp`) or profile gate path (`StartResp`, e.g. `tasks.build`).
    pub cursor: String,
    /// Assigned role name (`None` for mechanical command/call gates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Execution mode (`inline` or `subagent`) when `role` is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RoleExecutionMode>,
    /// Whether to reuse an existing session across rejections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_on_reject: Option<bool>,
    /// Ordered recommended models (`model` or `model:reasoning`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelSpec>,
    /// Coordinator-facing role guidance (`roles.<name>.guidance`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// Effective command slot name when the gate executes a command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Domain action (`<domain>.<action>`) when the gate invokes a provider call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
}

impl<'de> Deserialize<'de> for ModelSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct MapRepr {
            model: String,
            #[serde(default)]
            reasoning: Option<String>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Map(MapRepr),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Str(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(de::Error::custom("model identifier cannot be empty"));
                }
                if let Some((model, reasoning)) = trimmed.split_once(':') {
                    let m = model.trim();
                    let r = reasoning.trim();
                    if m.is_empty() || r.is_empty() {
                        return Err(de::Error::custom(
                            "model string with ':' requires non-empty model and reasoning",
                        ));
                    }
                    Ok(Self {
                        model: m.to_owned(),
                        reasoning: Some(r.to_owned()),
                    })
                } else {
                    Ok(Self {
                        model: trimmed.to_owned(),
                        reasoning: None,
                    })
                }
            }
            Repr::Map(MapRepr { model, reasoning }) => {
                let m = model.trim();
                if m.is_empty() {
                    return Err(de::Error::custom("model identifier cannot be empty"));
                }
                let reasoning = reasoning
                    .map(|r| r.trim().to_owned())
                    .filter(|r| !r.is_empty());
                Ok(Self {
                    model: m.to_owned(),
                    reasoning,
                })
            }
        }
    }
}

/// Role definition controlling execution mode, rejection reuse, models, and external runner.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoleDef {
    /// Execution mode (`subagent` or `inline`).
    pub mode: RoleExecutionMode,
    /// Explicit boolean controlling session reuse across rejections (defaults to `false`).
    pub reuse_on_reject: bool,
    /// Ordered model recommendations.
    pub models: Vec<ModelSpec>,
    /// Advisory role guidance text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// Optional external runner command template for `acdd run`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Optional prompt template rendered to the runner's stdin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_template: Option<String>,
}

/// Field-wise patch for `RoleDef`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RolePatch {
    /// Override `mode`.
    pub mode: PatchField<RoleExecutionMode>,
    /// Override `reuse_on_reject`.
    pub reuse_on_reject: PatchField<bool>,
    /// Replace `models`.
    pub models: PatchField<Vec<ModelSpec>>,
    /// Override or clear `guidance`.
    pub guidance: PatchField<String>,
    /// Override or clear `runner`.
    pub runner: PatchField<String>,
    /// Override or clear `prompt_template`.
    pub prompt_template: PatchField<String>,
}

/// Configuration of test-target filtering (`test_filter:`) on a command definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum TestFilterConfig {
    /// Explicitly enable (`true`, using `"{test}"`) or disable (`false`) `--test`.
    Enabled(bool),
    /// Filter template containing `{test}` (e.g. `"-- {test}"` or `"{test}"`).
    Template(String),
}

impl<'de> Deserialize<'de> for TestFilterConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Bool(bool),
            Str(String),
        }

        match Helper::deserialize(deserializer)? {
            Helper::Bool(b) => Ok(Self::Enabled(b)),
            Helper::Str(s) => Ok(Self::Template(s)),
        }
    }
}

/// Detailed shell command configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailedCommandDef {
    /// Shell command template (may contain at most one `{test_filter}`).
    pub command: String,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional per-command timeout override in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// Optional `--test` filter template or boolean flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_filter: Option<TestFilterConfig>,
    /// Optional path-pattern to `{scope_target}` argument mappings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scope_args_by_pattern: BTreeMap<String, String>,
}

/// Pattern-matched command entry preserving YAML declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternCommandEntry {
    /// Workspace-relative glob pattern matched against node scope paths.
    pub pattern: String,
    /// Shell command template executed when this pattern wins.
    pub command: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional timeout override in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// Optional `--test` filter template or boolean flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_filter: Option<TestFilterConfig>,
    /// Optional path-pattern to `{scope_target}` argument mappings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scope_args_by_pattern: BTreeMap<String, String>,
}

/// Command slot definition (`unbound: true`, `Script`, `Detailed`, or `Pattern`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommandDef {
    /// Abstract command slot declared in `code.yaml` requiring project override before run binding.
    Unbound {
        /// Must be `true`.
        unbound: bool,
        /// Optional description explaining what command to bind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Simple shell command string.
    Script(String),
    /// Structured command configuration.
    Detailed(Box<DetailedCommandDef>),
    /// Ordered pattern-matched command entries (selected by glob specificity, then YAML order).
    Pattern(Vec<PatternCommandEntry>),
}

impl<'de> Deserialize<'de> for CommandDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct UnboundRepr {
            unbound: bool,
            #[serde(default)]
            description: Option<String>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PatternMapBlock {
            by_pattern: ByPatternPairs,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            timeout_seconds: Option<u64>,
            #[serde(default)]
            test_filter: Option<TestFilterConfig>,
            #[serde(default)]
            scope_args_by_pattern: BTreeMap<String, String>,
        }

        struct ByPatternPairs(Vec<(String, String)>);

        impl<'de> Deserialize<'de> for ByPatternPairs {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct PairsVisitor;
                impl<'de> Visitor<'de> for PairsVisitor {
                    type Value = ByPatternPairs;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("an ordered mapping of glob pattern to command string")
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: MapAccess<'de>,
                    {
                        let mut out = Vec::new();
                        while let Some((k, v)) = map.next_entry::<String, String>()? {
                            out.push((k, v));
                        }
                        Ok(ByPatternPairs(out))
                    }
                }
                deserializer.deserialize_map(PairsVisitor)
            }
        }

        let val = serde_yaml::Value::deserialize(deserializer)?;
        match &val {
            serde_yaml::Value::String(s) => Ok(Self::Script(s.clone())),
            serde_yaml::Value::Sequence(_) => {
                let entries: Vec<PatternCommandEntry> =
                    serde_yaml::from_value(val).map_err(de::Error::custom)?;
                Ok(Self::Pattern(entries))
            }
            serde_yaml::Value::Mapping(map) => {
                let has_unbound = map.contains_key(serde_yaml::Value::String("unbound".into()));
                let has_by_pattern =
                    map.contains_key(serde_yaml::Value::String("by_pattern".into()));
                if has_unbound {
                    let u: UnboundRepr =
                        serde_yaml::from_value(val).map_err(de::Error::custom)?;
                    if !u.unbound {
                        return Err(de::Error::custom(
                            "unbound command slot must set 'unbound: true' or provide a command",
                        ));
                    }
                    Ok(Self::Unbound {
                        unbound: u.unbound,
                        description: u.description,
                    })
                } else if has_by_pattern {
                    let block: PatternMapBlock =
                        serde_yaml::from_value(val).map_err(de::Error::custom)?;
                    let entries = block
                        .by_pattern
                        .0
                        .into_iter()
                        .map(|(pattern, command)| PatternCommandEntry {
                            pattern,
                            command,
                            description: block.description.clone(),
                            timeout_seconds: block.timeout_seconds,
                            test_filter: block.test_filter.clone(),
                            scope_args_by_pattern: block.scope_args_by_pattern.clone(),
                        })
                        .collect();
                    Ok(Self::Pattern(entries))
                } else {
                    let detailed: DetailedCommandDef =
                        serde_yaml::from_value(val).map_err(de::Error::custom)?;
                    Ok(Self::Detailed(Box::new(detailed)))
                }
            }
            other => Err(de::Error::custom(format!(
                "invalid command definition: {other:?}"
            ))),
        }
    }
}

/// Built-in closed proof kinds admitted in `v2` profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofKind {
    /// No typed proof required beyond gate execution/submission rules.
    None,
    /// Passing command verification proof (`exit_code == 0`).
    Command,
    /// Domain provider `call: <domain>.<action>` proof.
    Call,
    /// Independent cognitive review proof requiring `role` + non-empty `contours`.
    Review,
}

impl ProofKind {
    /// Wire name of this proof kind.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Command => "command",
            Self::Call => "call",
            Self::Review => "review",
        }
    }

    fn from_name(raw: &str) -> Result<Option<Self>, &'static str> {
        match raw {
            "none" => Ok(Some(Self::None)),
            "command" => Ok(Some(Self::Command)),
            "call" => Ok(Some(Self::Call)),
            "review" => Ok(Some(Self::Review)),
            "contract" => Err("legacy proof kind 'contract' is removed in v0.6.2 (author seam tests in build)"),
            "seam_test" | "delivery" => Err("legacy proof kinds 'seam_test' and 'delivery' are removed in v2 profiles"),
            _ => Ok(None),
        }
    }
}

/// Gate proof specification (`none`, `command`, `call`, `review`, catalog name, or `{ scheme }`).
#[derive(Debug, Clone, PartialEq)]
pub enum ProofDef {
    /// Built-in proof behavior (defaults to `ProofKind::None`).
    Kind(ProofKind),
    /// Inline recursive JSON schema (`proof: { scheme: ... }`).
    Scheme {
        /// Evidence schema validated against `--evidence`.
        scheme: Box<SchemaDef>,
    },
    /// Reference to a named schema in `profile.proofs`.
    Named(String),
    /// Catalog proof resolved during profile resolution.
    ResolvedNamed {
        /// Catalog entry name.
        name: String,
        /// Resolved schema from `profile.proofs`.
        schema: Box<SchemaDef>,
    },
}

impl Default for ProofDef {
    fn default() -> Self {
        Self::Kind(ProofKind::None)
    }
}

impl Serialize for ProofDef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Kind(k) => serializer.serialize_str(k.as_str()),
            Self::Named(name) | Self::ResolvedNamed { name, .. } => serializer.serialize_str(name),
            Self::Scheme { scheme } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("scheme", scheme)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ProofDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SchemeRepr {
            scheme: Box<SchemaDef>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Null,
            Name(String),
            Scheme(SchemeRepr),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Null => Ok(Self::Kind(ProofKind::None)),
            Repr::Name(raw) => {
                let name = raw.trim();
                if name.is_empty() {
                    return Err(de::Error::custom("proof name cannot be empty"));
                }
                match ProofKind::from_name(name) {
                    Ok(Some(kind)) => Ok(Self::Kind(kind)),
                    Ok(None) => Ok(Self::Named(name.to_owned())),
                    Err(msg) => Err(de::Error::custom(msg)),
                }
            }
            Repr::Scheme(SchemeRepr { scheme }) => Ok(Self::Scheme { scheme }),
        }
    }
}

/// Reusable schema definition in `profile.proofs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofCatalogDef {
    /// Human-readable purpose description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Evidence schema definition.
    pub schema: SchemaDef,
}

/// Guidance slot content in `profile.guidance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GuidanceEntry {
    /// Plain text guidance string.
    Text(String),
    /// Structured guidance with description and operational criteria.
    Structured(StructuredGuidance),
}

/// Structured guidance entry with description and criteria list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredGuidance {
    /// Short purpose statement.
    pub description: String,
    /// Concrete operational criteria.
    #[serde(default)]
    pub criteria: Vec<String>,
}

impl GuidanceEntry {
    /// Render this guidance entry deterministically into Markdown (`###` heading + bullets).
    pub fn render(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Structured(s) => {
                if s.criteria.is_empty() {
                    return format!("### {}", s.description);
                }
                let mut out = format!("### {}\n", s.description);
                for criterion in &s.criteria {
                    out.push_str("\n- ");
                    out.push_str(criterion);
                }
                out
            }
        }
    }
}

/// Guidance item declared on a `GateDef`: either a strict slot key or `{ inline: "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GateGuidanceRef {
    /// Strict key lookup into `profile.guidance`.
    Slot(String),
    /// Explicit inline guidance string.
    Inline(InlineGuidance),
}

/// Inline guidance payload `{ inline: "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineGuidance {
    /// Inline guidance text.
    pub inline: String,
}

/// Review contour criteria definition in `profile.contours`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContourDef {
    /// Short contour description.
    pub description: String,
    /// Concrete inspection criteria for this contour.
    pub criteria: Vec<String>,
}

/// Domain check reference on a `GateDef` (`"domain.check"` or `{ check, with? }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckRef {
    /// Qualified check name (`<domain>.<check>`).
    pub check: String,
    /// Optional parameters validated against `CheckCapability.input_schema`.
    #[serde(default = "default_empty_object")]
    pub with: Value,
}

fn default_empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

impl<'de> Deserialize<'de> for CheckRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct MapRepr {
            check: String,
            #[serde(default = "default_empty_object")]
            with: Value,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Str(String),
            Map(MapRepr),
        }

        match Helper::deserialize(deserializer)? {
            Helper::Str(check) => Ok(Self {
                check,
                with: default_empty_object(),
            }),
            Helper::Map(m) => Ok(Self {
                check: m.check,
                with: m.with,
            }),
        }
    }
}

/// Persistence, immutability, archive, prune, and report policy on a `GateDef`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SavePolicy {
    /// Post-acceptance save action (`none` or `commit`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<SaveAction>,
    /// Require working tree at submit to match claim tree.
    pub immutable: bool,
    /// Trigger archive finalization (`root_gate` only).
    pub archive: bool,
    /// Trigger run pruning after archive (`root_gate` only).
    pub prune: bool,
    /// Optional workspace-relative report output path rendered on acceptance (`{run_id}`, `{run_name}`, `{gate_id}`, `{spec_id}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
}

/// Field-wise patch representation for `GateDef.save` (supports scalar shorthand `save: commit`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavePolicyPatch {
    /// Override or clear `post`.
    pub post: PatchField<SaveAction>,
    /// Override `immutable`.
    pub immutable: PatchField<bool>,
    /// Override `archive`.
    pub archive: PatchField<bool>,
    /// Override `prune`.
    pub prune: PatchField<bool>,
    /// Override or clear `report`.
    pub report: PatchField<String>,
}

impl<'de> Deserialize<'de> for SavePolicyPatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct MapRepr {
            post: PatchField<SaveAction>,
            immutable: PatchField<bool>,
            archive: PatchField<bool>,
            prune: PatchField<bool>,
            report: PatchField<String>,
        }

        impl Default for MapRepr {
            fn default() -> Self {
                Self {
                    post: PatchField::Unset,
                    immutable: PatchField::Unset,
                    archive: PatchField::Unset,
                    prune: PatchField::Unset,
                    report: PatchField::Unset,
                }
            }
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Scalar(SaveAction),
            Map(MapRepr),
        }

        match Helper::deserialize(deserializer)? {
            Helper::Scalar(action) => Ok(Self {
                post: PatchField::Set(action),
                ..Default::default()
            }),
            Helper::Map(m) => Ok(Self {
                post: m.post,
                immutable: m.immutable,
                archive: m.archive,
                prune: m.prune,
                report: m.report,
            }),
        }
    }
}

/// Global byte and timeout limits for logs, guidance, diagnostics, and metadata (`limits:`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimits {
    /// Reserved maximum bytes for mandatory `contract + guidance` in `ClaimResp`/`ShowResp`.
    pub max_guidance_bytes: usize,
    /// Maximum log bytes persisted per trace log file.
    pub max_log_bytes: usize,
    /// Maximum raw stdout/stderr capture bytes held in memory.
    pub max_capture_bytes: usize,
    /// Maximum single log line byte length before truncation.
    pub max_line_bytes: usize,
    /// Maximum inline diagnostic snippet bytes included in responses.
    pub diagnostic_inline_bytes: usize,
    /// Maximum metadata bytes stored in spec/receipt summaries.
    pub max_metadata_bytes: usize,
    /// Maximum trace summary bytes persisted per trace.
    pub max_trace_summary_bytes: usize,
    /// Maximum wall-clock seconds before killing a gate command process group.
    pub max_command_timeout_seconds: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_guidance_bytes: 32_768,
            max_log_bytes: 1_048_576,
            max_capture_bytes: 262_144,
            max_line_bytes: 65_536,
            diagnostic_inline_bytes: 4_096,
            max_metadata_bytes: 2_048,
            max_trace_summary_bytes: 2_048,
            max_command_timeout_seconds: 1_800,
        }
    }
}

/// Field-wise patch for `ResourceLimits`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimitsPatch {
    /// Override `max_guidance_bytes`.
    pub max_guidance_bytes: Option<usize>,
    /// Override `max_log_bytes`.
    pub max_log_bytes: Option<usize>,
    /// Override `max_capture_bytes`.
    pub max_capture_bytes: Option<usize>,
    /// Override `max_line_bytes`.
    pub max_line_bytes: Option<usize>,
    /// Override `diagnostic_inline_bytes`.
    pub diagnostic_inline_bytes: Option<usize>,
    /// Override `max_metadata_bytes`.
    pub max_metadata_bytes: Option<usize>,
    /// Override `max_trace_summary_bytes`.
    pub max_trace_summary_bytes: Option<usize>,
    /// Override `max_command_timeout_seconds`.
    pub max_command_timeout_seconds: Option<u64>,
}

/// Field-wise patch for `SubsystemRegistry` (`modules:`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubsystemRegistryPatch {
    /// Override preset.
    pub profile: Option<SubsystemPreset>,
    /// Replace `enabled` set.
    pub enabled: PatchField<BTreeSet<String>>,
    /// Replace `disabled` set.
    pub disabled: PatchField<BTreeSet<String>>,
    /// Replace `required` set.
    pub required: PatchField<BTreeSet<String>>,
    /// Key-wise merge on `domains` (`null` deletes domain toggle).
    pub domains: BTreeMap<String, Option<bool>>,
}

/// Reusable subgraph template declared in `profile.gate_templates`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateTemplate {
    /// Child gates instantiated by this template.
    pub gates: Vec<RawGateDef>,
    /// Default command slot inherited by expanded gates if unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Guidance slots/inline entries attached by default.
    pub guidance: Vec<GateGuidanceRef>,
    /// Default `max_attempts` override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
}

/// Task subgraph block inside a `- tasks:` node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasksBlock {
    /// Optional `when:` predicate selecting tasks for this track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Ordered gates instantiated for every matching non-cancelled task.
    pub gates: Vec<GateDef>,
}

/// Resolved gate node definition inside a `ResolvedGateProfile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateDef {
    /// Gate identifier (empty only for anonymous `- tasks:` blocks).
    pub id: String,
    /// Optional role assigned to execute/review this gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Optional named command slot from `profile.commands`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Optional domain action `<domain>.<action>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// Optional parameters passed to `call`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with: Option<Value>,
    /// Domain check verifiers (`[<domain>.<check>]`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckRef>,
    /// Domain context lenses (`[<domain>.<lens>]`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    /// Nested child gates (making this node a container).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateDef>,
    /// Nested per-task lifecycle block (`- tasks:`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks: Option<TasksBlock>,
    /// Template name expanded into this gate (retained for diagnostics).
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    pub use_template: Option<String>,
    /// Conditional predicate (allowed on `- tasks:` and `task_gate`, forbidden on `root_gate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Declared proof contract (`none` when omitted).
    pub proof: ProofDef,
    /// Optional per-gate tool allowlist override (`None` inherits `profile.default_tools`; `Some([])` clears).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Tools referenced in `tools_remove` patches, retained strictly for fail-closed validation.
    #[serde(skip)]
    pub(crate) tools_removed_for_validation: Vec<String>,
    /// Guidance slot references and/or inline entries.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub guidance: Vec<GateGuidanceRef>,
    /// Review contour names required on `proof: review`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contours: Vec<String>,
    /// Explicit admitted write scope globs for this gate (e.g. `polish`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Persistence, immutability, archive, prune, and commit note settings.
    pub save: SavePolicy,
    /// Maximum failure/rejection attempts before escalation (`>= 1`, default `3`).
    pub max_attempts: u32,
    /// Whether passing this gate produces a durable receipt and closes the container.
    pub receipt: bool,
    /// Gate selectors whose claiming worker must differ from this gate's worker.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub independent_from: Vec<GateSelector>,
    /// Predecessor gates whose accepted candidate snapshots must match at submit.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub need: Vec<GateSelector>,
    /// Target sibling gate activated on pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pass: Option<GateId>,
    /// Target sibling gate activated on reject/failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail: Option<GateId>,
    /// Target sibling gate activated on `architectural_drift` rejection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_drift: Option<GateId>,
}

impl PartialEq for GateDef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.role == other.role
            && self.command == other.command
            && self.call == other.call
            && self.with == other.with
            && self.checks == other.checks
            && self.context == other.context
            && self.gates == other.gates
            && self.tasks == other.tasks
            && self.use_template == other.use_template
            && self.when == other.when
            && self.proof == other.proof
            && self.tools == other.tools
            && self.guidance == other.guidance
            && self.contours == other.contours
            && self.scope == other.scope
            && self.save == other.save
            && self.max_attempts == other.max_attempts
            && self.receipt == other.receipt
            && self.independent_from == other.independent_from
            && self.need == other.need
            && self.pass == other.pass
            && self.fail == other.fail
            && self.fail_drift == other.fail_drift
    }
}

impl Default for GateDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            role: None,
            command: None,
            call: None,
            with: None,
            checks: Vec::new(),
            context: Vec::new(),
            gates: Vec::new(),
            tasks: None,
            use_template: None,
            when: None,
            proof: ProofDef::default(),
            tools: None,
            tools_removed_for_validation: Vec::new(),
            guidance: Vec::new(),
            contours: Vec::new(),
            scope: Vec::new(),
            save: SavePolicy::default(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            receipt: false,
            independent_from: Vec::new(),
            need: Vec::new(),
            pass: None,
            fail: None,
            fail_drift: None,
        }
    }
}

impl GateDef {
    /// Compute the deterministic RFC-8785 SHA-256 digest of this resolved `GateDef`.
    pub fn definition_digest(&self) -> Result<String, StateError> {
        let value = serde_json::to_value(self).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize GateDef for digest: {err}"),
            )
        })?;
        Ok(canonical_json_sha256(&value))
    }

    /// Returns `true` if this entry represents a `- tasks:` block.
    pub fn is_tasks_block(&self) -> bool {
        self.tasks.is_some()
    }
}

/// Raw gate entry in a YAML profile supporting patch cursors, `after`/`before`, `tools_add`/`tools_remove`, and `null` clearing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawGateDef {
    /// Gate ID or patch cursor (`"tasks.build"`, `"blueprint_synthesis.review"`).
    pub id: Option<String>,
    /// Patch insertion anchor: insert new gate immediately after `<gate_id>`.
    pub after: Option<String>,
    /// Patch insertion anchor: insert new gate immediately before `<gate_id>`.
    pub before: Option<String>,
    /// Role assignment.
    #[serde(skip_serializing)]
    pub role: PatchField<String>,
    /// Command slot assignment.
    #[serde(skip_serializing)]
    pub command: PatchField<String>,
    /// Domain action `<domain>.<action>`.
    #[serde(skip_serializing)]
    pub call: PatchField<String>,
    /// Action input parameters.
    #[serde(skip_serializing)]
    pub with: PatchField<Value>,
    /// Domain checks list.
    #[serde(skip_serializing)]
    pub checks: PatchField<Vec<CheckRef>>,
    /// Domain context lenses list.
    #[serde(skip_serializing)]
    pub context: PatchField<Vec<String>>,
    /// Nested child gates / patches.
    #[serde(skip_serializing)]
    pub gates: PatchField<Vec<RawGateItem>>,
    /// Nested `- tasks:` block.
    #[serde(skip_serializing)]
    pub tasks: Option<RawTasksBlock>,
    /// Template reference (`use:`).
    #[serde(rename = "use", skip_serializing)]
    pub use_template: PatchField<String>,
    /// Conditional `when:` expression.
    #[serde(skip_serializing)]
    pub when: PatchField<String>,
    /// Proof definition.
    #[serde(skip_serializing)]
    pub proof: PatchField<ProofDef>,
    /// Full replacement for `tools`.
    #[serde(skip_serializing)]
    pub tools: PatchField<Vec<String>>,
    /// Incremental additions to inherited `tools` (or `default_tools`).
    #[serde(skip_serializing)]
    pub tools_add: Vec<String>,
    /// Incremental removals from inherited `tools` (or `default_tools`).
    #[serde(skip_serializing)]
    pub tools_remove: Vec<String>,
    /// Guidance slots/inline entries.
    #[serde(skip_serializing)]
    pub guidance: PatchField<Vec<GateGuidanceRef>>,
    /// Review contour names.
    #[serde(skip_serializing)]
    pub contours: PatchField<Vec<String>>,
    /// Explicit admitted scope globs.
    #[serde(skip_serializing)]
    pub scope: PatchField<Vec<String>>,
    /// Field-wise save policy patch.
    #[serde(skip_serializing)]
    pub save: Option<SavePolicyPatch>,
    /// Retry limit override.
    #[serde(skip_serializing)]
    pub max_attempts: PatchField<u32>,
    /// Receipt flag override.
    #[serde(skip_serializing)]
    pub receipt: PatchField<bool>,
    /// Independence selector list.
    #[serde(skip_serializing)]
    pub independent_from: PatchField<Vec<GateSelector>>,
    /// Predecessor snapshot dependency list.
    #[serde(skip_serializing)]
    pub need: PatchField<Vec<GateSelector>>,
    /// Pass transition target.
    #[serde(skip_serializing)]
    pub pass: PatchField<GateId>,
    /// Fail transition target.
    #[serde(skip_serializing)]
    pub fail: PatchField<GateId>,
    /// Architectural-drift fail transition target.
    #[serde(skip_serializing)]
    pub fail_drift: PatchField<GateId>,
}

/// Raw `- tasks:` block in YAML.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawTasksBlock {
    /// Optional `when:` predicate selecting tasks for this track.
    pub when: PatchField<String>,
    /// Child gates or patch items inside this tasks block.
    pub gates: Vec<RawGateItem>,
}

/// Single element in a YAML `gates:` sequence: either a gate/patch object or `<id>: null` deletion.
#[derive(Debug, Clone, PartialEq)]
pub enum RawGateItem {
    /// Gate definition, container, `- tasks:` block, or patch object.
    Gate(Box<RawGateDef>),
    /// Explicit gate deletion (`- <id_or_cursor>: null`) permitted inside `gates_patch`.
    Delete(String),
}

impl<'de> Deserialize<'de> for RawGateItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let val = serde_yaml::Value::deserialize(deserializer)?;
        let serde_yaml::Value::Mapping(ref map) = val else {
            return Err(de::Error::custom("each gates entry must be a YAML mapping"));
        };
        if map.len() == 1 {
            if let Some((serde_yaml::Value::String(key), serde_yaml::Value::Null)) = map.iter().next()
            {
                if key == "tasks" || key.is_empty() {
                    return Err(de::Error::custom(format!(
                        "invalid gate deletion target '{key}'"
                    )));
                }
                GateSelector::from_str_or_err(key).map_err(de::Error::custom)?;
                return Ok(Self::Delete(key.clone()));
            }
        }
        let gate: RawGateDef = serde_yaml::from_value(val).map_err(de::Error::custom)?;
        Ok(Self::Gate(Box::new(gate)))
    }
}

trait GateSelectorExt {
    fn from_str_or_err(raw: &str) -> Result<GateSelector, String>;
}

impl GateSelectorExt for GateSelector {
    fn from_str_or_err(raw: &str) -> Result<GateSelector, String> {
        raw.parse::<GateSelector>().map_err(|e| e.to_string())
    }
}

/// Strictly parsed single version-2 profile YAML document (`#[serde(deny_unknown_fields)]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedProfileFile {
    /// Schema version (must equal `2`).
    pub version: u32,
    /// Optional inheritance source (`base | code | audit | none | <path>[:<sha256_prefix>]`).
    #[serde(default)]
    pub inherit: Option<String>,
    /// Field-wise Git policy overrides.
    #[serde(default)]
    pub git: Option<GitPolicyPatch>,
    /// Top-level default save action for terminal root gate (`none | commit`).
    #[serde(default)]
    pub save: PatchField<SaveAction>,
    /// Allowed task tags (`"default"` is always implicitly permitted).
    #[serde(default)]
    pub tags: PatchField<BTreeSet<String>>,
    /// Declarative project-protected path globs.
    #[serde(default)]
    pub protected_paths: PatchField<Vec<String>>,
    /// Default MCP tool allowlist inherited by gates omitting `tools:`.
    #[serde(default)]
    pub default_tools: PatchField<Vec<String>>,
    /// Key-wise role definitions (`null` deletes an inherited role).
    #[serde(default)]
    pub roles: BTreeMap<String, Option<RolePatch>>,
    /// Key-wise command slot definitions (`null` deletes an inherited command).
    #[serde(default)]
    pub commands: BTreeMap<String, Option<CommandDef>>,
    /// Key-wise path-pattern command argument mappings (`null` deletes a key).
    #[serde(default)]
    pub scope_args_by_pattern: BTreeMap<String, Option<String>>,
    /// Key-wise named proof schema catalog (`null` deletes an entry).
    #[serde(default)]
    pub proofs: BTreeMap<String, Option<ProofCatalogDef>>,
    /// Key-wise guidance slot catalog (`null` deletes a slot).
    #[serde(default)]
    pub guidance: BTreeMap<String, Option<GuidanceEntry>>,
    /// Key-wise review contour catalog (`null` deletes a contour).
    #[serde(default)]
    pub contours: BTreeMap<String, Option<ContourDef>>,
    /// Key-wise reusable gate templates (`null` deletes a template).
    #[serde(default)]
    pub gate_templates: BTreeMap<String, Option<GateTemplate>>,
    /// Subsystem/domain enablement overrides.
    #[serde(default)]
    pub modules: Option<SubsystemRegistryPatch>,
    /// Field-wise resource limit overrides.
    #[serde(default)]
    pub limits: Option<ResourceLimitsPatch>,
    /// Ordered root gates and `- tasks:` blocks replacing inherited gates.
    #[serde(default)]
    pub gates: Option<Vec<RawGateItem>>,
    /// Targeted patch entries modifying inherited gates by ID, cursor, `after:`/`before:`, or `<id>: null`.
    #[serde(default)]
    pub gates_patch: Option<Vec<RawGateItem>>,
}

/// Fully resolved, inherited, and template-expanded `GateProfile` persisted in `runs.profile_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResolvedGateProfile {
    /// Profile schema version (`2`).
    pub version: u32,
    /// Resolved profile identifier (e.g. `"base"`, `"code"`, `"audit"`, or workspace relative path).
    pub profile_id: String,
    /// Resolved Git isolation/worktree policy.
    pub git: GitPolicy,
    /// Default terminal root-gate persistence action.
    pub save: SaveAction,
    /// Admitted tags (`"default"` is always included).
    pub tags: BTreeSet<String>,
    /// Protected path globs forbidding agent writes.
    pub protected_paths: Vec<String>,
    /// Default tool allowlist inherited by gates that omit `tools:`.
    pub default_tools: Vec<String>,
    /// Resolved role definitions.
    pub roles: BTreeMap<String, RoleDef>,
    /// Resolved command slots.
    pub commands: BTreeMap<String, CommandDef>,
    /// Profile-level path-pattern scope arguments.
    pub scope_args_by_pattern: BTreeMap<String, String>,
    /// Resolved proof schema catalog.
    pub proofs: BTreeMap<String, ProofCatalogDef>,
    /// Resolved guidance slot catalog.
    pub guidance: BTreeMap<String, GuidanceEntry>,
    /// Resolved review contour catalog.
    pub contours: BTreeMap<String, ContourDef>,
    /// Resolved gate templates (already expanded into `gates`).
    pub gate_templates: BTreeMap<String, GateTemplate>,
    /// Resolved subsystem/domain enablement registry.
    pub modules: SubsystemRegistry,
    /// Resolved resource and timeout limits.
    pub limits: ResourceLimits,
    /// Resolved ordered workflow gate graph.
    pub gates: Vec<GateDef>,
}

impl Default for ResolvedGateProfile {
    fn default() -> Self {
        Self {
            version: 2,
            profile_id: "code".to_owned(),
            git: GitPolicy::default(),
            save: SaveAction::Commit,
            tags: BTreeSet::from(["default".to_owned()]),
            protected_paths: Vec::new(),
            default_tools: Vec::new(),
            roles: BTreeMap::new(),
            commands: BTreeMap::new(),
            scope_args_by_pattern: BTreeMap::new(),
            proofs: BTreeMap::new(),
            guidance: BTreeMap::new(),
            contours: BTreeMap::new(),
            gate_templates: BTreeMap::new(),
            modules: SubsystemRegistry::default(),
            limits: ResourceLimits::default(),
            gates: Vec::new(),
        }
    }
}

impl ResolvedGateProfile {
    /// Return the effective tool allowlist for `gate`, falling back to `self.default_tools` when `gate.tools` is `None`.
    pub fn effective_tools<'a>(&'a self, gate: &'a GateDef) -> &'a [String] {
        gate.tools.as_deref().unwrap_or(&self.default_tools)
    }

    /// Project coordinator-facing dispatch metadata for `gate` at `cursor`.
    pub fn gate_dispatch_info(
        &self,
        cursor: impl Into<String>,
        gate: &GateDef,
        include_guidance: bool,
    ) -> GateDispatchInfo {
        let role_def = gate.role.as_deref().and_then(|name| self.roles.get(name));
        GateDispatchInfo {
            cursor: cursor.into(),
            role: gate.role.clone(),
            mode: role_def.map(|r| r.mode),
            reuse_on_reject: role_def.map(|r| r.reuse_on_reject),
            models: role_def.map(|r| r.models.clone()).unwrap_or_default(),
            guidance: if include_guidance {
                role_def.and_then(|r| r.guidance.clone())
            } else {
                None
            },
            command: gate.command.clone(),
            call: gate.call.clone(),
        }
    }

    /// Build the ordered compact coordinator dispatch plan across all executable gates in this profile.
    pub fn dispatch_plan(&self) -> Vec<GateDispatchInfo> {
        let mut out = Vec::new();
        self.collect_dispatch_plan(&self.gates, None, &mut out);
        out
    }

    fn collect_dispatch_plan(
        &self,
        gates: &[GateDef],
        prefix: Option<&str>,
        out: &mut Vec<GateDispatchInfo>,
    ) {
        for gate in gates {
            if let Some(ref tb) = gate.tasks {
                let task_prefix = if gate.id.is_empty() {
                    "tasks".to_owned()
                } else if let Some(p) = prefix {
                    format!("{p}.{}", gate.id)
                } else {
                    gate.id.clone()
                };
                self.collect_dispatch_plan(&tb.gates, Some(&task_prefix), out);
                continue;
            }
            let cursor = match prefix {
                Some(p) => format!("{p}.{}", gate.id),
                None => gate.id.clone(),
            };
            if gate.gates.is_empty() {
                out.push(self.gate_dispatch_info(cursor, gate, false));
            } else {
                self.collect_dispatch_plan(&gate.gates, Some(&cursor), out);
            }
        }
    }

    /// Serialize this resolved profile into canonical JSON for storage in `runs.profile_json`.
    pub fn to_canonical_json(&self) -> Result<String, StateError> {
        let value = serde_json::to_value(self).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize ResolvedGateProfile: {err}"),
            )
        })?;
        Ok(canonical_json_string(&value))
    }
}

/// Parse a YAML string strictly into a `ParsedProfileFile`.
pub fn parse_profile(yaml: &str) -> Result<ParsedProfileFile, StateError> {
    let parsed: ParsedProfileFile = serde_yaml::from_str(yaml).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid v2 gate profile YAML: {err}"),
        )
    })?;
    if parsed.version != 2 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "unsupported gate profile version {}; expected version: 2",
                parsed.version
            ),
        ));
    }
    if parsed.gates.is_some() && parsed.gates_patch.is_some() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "'gates' and 'gates_patch' cannot both be declared in the same profile layer",
        ));
    }
    if parsed.gates_patch.is_some() && matches!(parsed.inherit.as_deref(), None | Some("none")) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "'gates_patch' requires inheriting a parent profile via 'inherit'",
        ));
    }
    Ok(parsed)
}
