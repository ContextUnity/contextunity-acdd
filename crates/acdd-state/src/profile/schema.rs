//! Version-2 `GateProfile` schema definitions, patch types, and strict `deny_unknown_fields` deserialization.

use crate::types::{
    canonical_json_sha256, CommitMode, IntegrateStrategy, IsolationMode, SaveAction,
    WorktreeLifecycle,
};
use acdd_core::subsystems::{SubsystemPreset, SubsystemRegistry};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateId, GateSelector};
use acdd_core::schema::SchemaDef;
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
    /// Returns `true` when the field was omitted in YAML.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

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

/// Strategy controlling how a child profile's `gates:` list combines with its inherited parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatesMergeMode {
    /// Replace the parent's `gates:` sequence completely when child specifies `gates:`.
    #[default]
    Replace,
    /// Patch inherited gates by ID, cursor (`tasks.build`), normalized `when:`, `after:`/`before:`, or `<id>: null`.
    Patch,
}

/// Flexible Git isolation, worktree, commit, and integration policy (`git:`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitPolicy {
    /// Workspace isolation strategy (`worktree` or `in_place`).
    pub isolation: IsolationMode,
    /// Worktree lifecycle ownership (`manual` or `managed`).
    pub worktrees: WorktreeLifecycle,
    /// Git commit mode (`plumbing`, `cli`, or `manual`).
    pub commit_mode: CommitMode,
    /// Branch integration strategy (`manual`, `rebase-ff`, `ff-only`, `no-ff`, `squash`).
    pub integrate: IntegrateStrategy,
}

impl Default for GitPolicy {
    fn default() -> Self {
        Self {
            isolation: IsolationMode::Worktree,
            worktrees: WorktreeLifecycle::Manual,
            commit_mode: CommitMode::Plumbing,
            integrate: IntegrateStrategy::Manual,
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
    /// Override `git.commit_mode`.
    pub commit_mode: Option<CommitMode>,
    /// Override `git.integrate`.
    pub integrate: Option<IntegrateStrategy>,
}

/// Execution mode for a configured role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleExecutionMode {
    /// Run the role as a separate worker/subagent.
    #[default]
    Subagent,
    /// Run the role inline in the active session.
    Inline,
}

/// Model recommendation entry within a `RoleDef` (`"model"`, `"model:reasoning"`, or `{ model, reasoning? }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelSpec {
    /// Model identifier.
    pub model: String,
    /// Optional reasoning effort level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl ModelSpec {
    /// Format as `"model:reasoning"` or `"model"`.
    pub fn formatted(&self) -> String {
        match &self.reasoning {
            Some(r) => format!("{}:{r}", self.model),
            None => self.model.clone(),
        }
    }
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

/// Regex patterns extracting passed and failed test counts from command output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCountsSpec {
    /// Regex with capture group 1 extracting passed test count.
    pub passed: String,
    /// Regex with capture group 1 extracting failed test count.
    pub failed: String,
}

/// `compile_errors:` matcher supporting either pattern list or `{ include, exclude }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CompileErrorsSpec {
    /// Plain list of regex patterns matching compile error lines.
    Patterns(Vec<String>),
    /// Include/exclude regex filter pair.
    IncludeExclude(CompileErrorsFilter),
}

impl Default for CompileErrorsSpec {
    fn default() -> Self {
        Self::Patterns(Vec::new())
    }
}

impl CompileErrorsSpec {
    /// Returns `true` if no include or exclude patterns are configured.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Patterns(items) => items.is_empty(),
            Self::IncludeExclude(filter) => filter.include.is_empty() && filter.exclude.is_empty(),
        }
    }
}

impl<'de> Deserialize<'de> for CompileErrorsSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Patterns(Vec<String>),
            IncludeExclude(CompileErrorsFilter),
        }

        match Helper::deserialize(deserializer)? {
            Helper::Patterns(v) => Ok(Self::Patterns(v)),
            Helper::IncludeExclude(f) => Ok(Self::IncludeExclude(f)),
        }
    }
}

/// Structured include/exclude regex filters for `compile_errors`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompileErrorsFilter {
    /// Regexes matching candidate compile error lines.
    pub include: Vec<String>,
    /// Regexes excluding false positives from `include` (e.g. `^error: test failed`).
    pub exclude: Vec<String>,
}

/// Regex rules extracting individual test target and test outcome names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputTestsSpec {
    /// Optional regex with capture group 1 extracting the test suite/target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Regex with capture group 1 extracting passed test names.
    pub passed: String,
    /// Optional regex with capture group 1 extracting failed test names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Optional regex with capture group 1 extracting ignored test names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<String>,
}

/// Declarative output parsing rules for a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandOutputSpec {
    /// Optional test count extraction patterns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<CommandCountsSpec>,
    /// Whether non-zero test count evidence is required on `proof: command`.
    pub require_counts: bool,
    /// Noise line regexes stripped from captured diagnostics.
    pub noise: Vec<String>,
    /// Compile error detection patterns or include/exclude filter.
    #[serde(skip_serializing_if = "CompileErrorsSpec::is_empty")]
    pub compile_errors: CompileErrorsSpec,
    /// Individual test outcome extraction regexes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<OutputTestsSpec>,
}

impl Default for CommandOutputSpec {
    fn default() -> Self {
        Self {
            counts: None,
            require_counts: true,
            noise: Vec::new(),
            compile_errors: CompileErrorsSpec::default(),
            tests: None,
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
    /// Optional output parser specification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<CommandOutputSpec>,
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
    /// Optional output parser specification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<CommandOutputSpec>,
}

/// Command slot definition (`unbound: true`, `Script`, `Detailed`, or `Pattern`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommandDef {
    /// Abstract command slot declared in `default.yaml` requiring project override before run binding.
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
            #[serde(default)]
            output: Option<CommandOutputSpec>,
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
                            output: block.output.clone(),
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
    /// Runtime-red contract proof requiring `command` + `expected_failure` + `test_filter`.
    Contract,
    /// Passing command verification proof (`exit_code == 0`, `tests_failed == 0`).
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
            Self::Contract => "contract",
            Self::Command => "command",
            Self::Call => "call",
            Self::Review => "review",
        }
    }

    fn from_name(raw: &str) -> Result<Option<Self>, &'static str> {
        match raw {
            "none" => Ok(Some(Self::None)),
            "contract" => Ok(Some(Self::Contract)),
            "command" => Ok(Some(Self::Command)),
            "call" => Ok(Some(Self::Call)),
            "review" => Ok(Some(Self::Review)),
            "seam_test" | "delivery" => Err("legacy proof kinds 'seam_test' and 'delivery' are removed in v2 profiles"),
            _ => Ok(None),
        }
    }
}

/// Gate proof specification (`none`, `contract`, `command`, `call`, `review`, catalog name, or `{ scheme }`).
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

impl ProofDef {
    /// Return the built-in `ProofKind`, or `None` for schema/catalog proofs.
    pub fn kind(&self) -> Option<ProofKind> {
        match self {
            Self::Kind(k) => Some(*k),
            Self::Scheme { .. } | Self::Named(_) | Self::ResolvedNamed { .. } => None,
        }
    }

    /// Return the referenced catalog name when this proof is `Named` or `ResolvedNamed`.
    pub fn catalog_name(&self) -> Option<&str> {
        match self {
            Self::Named(n) | Self::ResolvedNamed { name: n, .. } => Some(n.as_str()),
            _ => None,
        }
    }

    /// Return the resolved `SchemaDef` when inline or already resolved.
    pub fn inline_or_resolved_schema(&self) -> Option<&SchemaDef> {
        match self {
            Self::Scheme { scheme: schema } | Self::ResolvedNamed { schema, .. } => {
                Some(schema.as_ref())
            }
            _ => None,
        }
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
    /// Render this guidance entry deterministically into plain text.
    pub fn render(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Structured(s) => {
                let mut out = s.description.clone();
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

/// Expected failure signature required for `proof: contract`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExpectedFailureSpec {
    /// Accepted non-zero exit codes (e.g. `[1, 101, 134]`).
    pub exit_codes: Vec<i32>,
    /// Optional substring required in stderr.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_contains: Option<String>,
}

/// Field-wise patch for `ExpectedFailureSpec`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExpectedFailurePatch {
    /// Replace `exit_codes`.
    pub exit_codes: PatchField<Vec<i32>>,
    /// Override or clear `stderr_contains`.
    pub stderr_contains: PatchField<String>,
}

/// Persistence, immutability, archive, prune, and Git commit note policy on a `GateDef`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SavePolicy {
    /// Pre-execution save action (`none` or `snapshot` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre: Option<SaveAction>,
    /// Post-acceptance save action (`none`, `snapshot`, or `commit`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<SaveAction>,
    /// Require working tree at submit to match claim tree.
    pub immutable: bool,
    /// Trigger archive finalization (`root_gate` only).
    pub archive: bool,
    /// Trigger run pruning after archive (`root_gate` only).
    pub prune: bool,
    /// Key referencing `profile.git_commit_notes` attached when `post == commit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit_note: Option<String>,
}

/// Field-wise patch representation for `GateDef.save` (supports scalar shorthand `save: snapshot`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavePolicyPatch {
    /// Override or clear `pre`.
    pub pre: PatchField<SaveAction>,
    /// Override or clear `post`.
    pub post: PatchField<SaveAction>,
    /// Override `immutable`.
    pub immutable: PatchField<bool>,
    /// Override `archive`.
    pub archive: PatchField<bool>,
    /// Override `prune`.
    pub prune: PatchField<bool>,
    /// Override or clear `git_commit_note`.
    pub git_commit_note: PatchField<String>,
}

impl<'de> Deserialize<'de> for SavePolicyPatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct MapRepr {
            pre: PatchField<SaveAction>,
            post: PatchField<SaveAction>,
            immutable: PatchField<bool>,
            archive: PatchField<bool>,
            prune: PatchField<bool>,
            git_commit_note: PatchField<String>,
        }

        impl Default for MapRepr {
            fn default() -> Self {
                Self {
                    pre: PatchField::Unset,
                    post: PatchField::Unset,
                    immutable: PatchField::Unset,
                    archive: PatchField::Unset,
                    prune: PatchField::Unset,
                    git_commit_note: PatchField::Unset,
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
                pre: m.pre,
                post: m.post,
                immutable: m.immutable,
                archive: m.archive,
                prune: m.prune,
                git_commit_note: m.git_commit_note,
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

/// Output format for `git_commit_notes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitNoteFormat {
    /// Human-readable Markdown note.
    #[default]
    Markdown,
    /// Machine-readable JSON note.
    Json,
}

/// Canonical telemetry sections allowed in `git_commit_notes.<name>.sections` (excluding legacy `ast_delta`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitNoteSection {
    /// Elapsed wall-clock duration.
    Duration,
    /// Completed task receipt summary.
    TasksSummary,
    /// Review cycle count summary.
    ReviewCycles,
    /// Diff file/line breakdown.
    DiffBreakdown,
    /// Test suite counts.
    TestSuite,
    /// Run/worktree topology summary.
    Topology,
}

/// Configuration for one named Git commit note template in `git_commit_notes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitCommitNotePolicy {
    /// Git notes ref name (default `refs/notes/acdd`).
    pub ref_name: String,
    /// Maximum rendered note bytes (default `2048`).
    pub max_bytes: usize,
    /// Rendering format (`markdown` or `json`).
    pub format: GitCommitNoteFormat,
    /// Included telemetry sections.
    pub sections: Vec<GitCommitNoteSection>,
}

impl Default for GitCommitNotePolicy {
    fn default() -> Self {
        Self {
            ref_name: "refs/notes/acdd".to_owned(),
            max_bytes: 2048,
            format: GitCommitNoteFormat::Markdown,
            sections: vec![
                GitCommitNoteSection::Duration,
                GitCommitNoteSection::TasksSummary,
                GitCommitNoteSection::ReviewCycles,
                GitCommitNoteSection::DiffBreakdown,
                GitCommitNoteSection::TestSuite,
                GitCommitNoteSection::Topology,
            ],
        }
    }
}

/// Field-wise patch for `GitCommitNotePolicy`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitCommitNotePolicyPatch {
    /// Override `ref_name`.
    pub ref_name: PatchField<String>,
    /// Override `max_bytes`.
    pub max_bytes: PatchField<usize>,
    /// Override `format`.
    pub format: PatchField<GitCommitNoteFormat>,
    /// Replace `sections`.
    pub sections: PatchField<Vec<GitCommitNoteSection>>,
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
    /// Resolved tool allowlist (inherited from `default_tools` if omitted, then modified by `tools_add`/`tools_remove`).
    pub tools: Vec<String>,
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
    /// Expected runtime-red failure signature (allowed only on `proof: contract`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_failure: Option<ExpectedFailureSpec>,
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
            && self.expected_failure == other.expected_failure
            && self.max_attempts == other.max_attempts
            && self.receipt == other.receipt
            && self.independent_from == other.independent_from
            && self.need == other.need
            && self.pass == other.pass
            && self.fail == other.fail
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
            tools: Vec::new(),
            tools_removed_for_validation: Vec::new(),
            guidance: Vec::new(),
            contours: Vec::new(),
            scope: Vec::new(),
            save: SavePolicy::default(),
            expected_failure: None,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            receipt: false,
            independent_from: Vec::new(),
            need: Vec::new(),
            pass: None,
            fail: None,
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
        canonical_json_sha256(&value)
    }

    /// Returns `true` if this entry represents a `- tasks:` block.
    pub fn is_tasks_block(&self) -> bool {
        self.tasks.is_some()
    }

    /// Returns `true` if this entry is a container node holding child gates.
    pub fn is_container(&self) -> bool {
        !self.gates.is_empty() || self.tasks.is_some()
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
    /// Field-wise expected_failure patch.
    #[serde(skip_serializing)]
    pub expected_failure: PatchField<ExpectedFailurePatch>,
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
    /// Explicit gate deletion (`- <id_or_cursor>: null`) permitted under `gates_merge: patch`.
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
    /// Optional inheritance source (`default | code | in_place | workspace | none | <path>[:<sha256_prefix>]`).
    #[serde(default)]
    pub inherit: Option<String>,
    /// How `gates:` combines with the inherited parent (`replace` or `patch`).
    #[serde(default)]
    pub gates_merge: GatesMergeMode,
    /// Field-wise Git policy overrides.
    #[serde(default)]
    pub git: Option<GitPolicyPatch>,
    /// Top-level default save action for terminal root gate (`none | snapshot | commit`).
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
    /// Key-wise Git commit note policies (`null` deletes a policy).
    #[serde(default)]
    pub git_commit_notes: BTreeMap<String, Option<GitCommitNotePolicyPatch>>,
    /// Ordered root gates and `- tasks:` blocks (or patch entries under `gates_merge: patch`).
    #[serde(default)]
    pub gates: Option<Vec<RawGateItem>>,
}

/// Fully resolved, inherited, and template-expanded `GateProfile` persisted in `runs.profile_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResolvedGateProfile {
    /// Profile schema version (`2`).
    pub version: u32,
    /// Resolved profile identifier (e.g. `"default"`, `"code"`, `"in_place"`, or workspace relative path).
    pub profile_id: String,
    /// Resolved Git isolation/worktree/commit/integration policy.
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
    /// Resolved Git commit note policies.
    pub git_commit_notes: BTreeMap<String, GitCommitNotePolicy>,
    /// Resolved ordered workflow gate graph.
    pub gates: Vec<GateDef>,
}

impl Default for ResolvedGateProfile {
    fn default() -> Self {
        Self {
            version: 2,
            profile_id: "default".to_owned(),
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
            git_commit_notes: BTreeMap::new(),
            gates: Vec::new(),
        }
    }
}

impl ResolvedGateProfile {
    /// Serialize this resolved profile into canonical JSON for storage in `runs.profile_json`.
    pub fn to_canonical_json(&self) -> Result<String, StateError> {
        let value = serde_json::to_value(self).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize ResolvedGateProfile: {err}"),
            )
        })?;
        crate::types::canonical_json_string(&value)
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
    Ok(parsed)
}
