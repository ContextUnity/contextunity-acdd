use crate::errors::{ErrorCode, StateError};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

/// Error returned when parsing or validating an identifier, OID, cursor, or selector fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(String);

impl IdError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IdError {}

impl From<IdError> for StateError {
    fn from(err: IdError) -> Self {
        StateError::new(ErrorCode::ProfileSchemaInvalid, err.0)
    }
}

/// Validate `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`.
pub fn validate_standard_ident(raw: &str, kind: &'static str) -> Result<(), IdError> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return Err(IdError::new(format!(
            "{kind} '{raw}' must be 1..=64 ASCII characters matching ^[A-Za-z0-9][A-Za-z0-9_-]{{0,63}}$"
        )));
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(IdError::new(format!(
            "{kind} '{raw}' must start with an ASCII alphanumeric character"
        )));
    }
    if !bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
    {
        return Err(IdError::new(format!(
            "{kind} '{raw}' contains invalid characters; only [A-Za-z0-9_-] are permitted"
        )));
    }
    Ok(())
}

/// Return whether `raw` is a strictly lowercase 40-char (SHA-1) or 64-char (SHA-256) Git OID.
pub fn is_valid_full_oid(raw: &str) -> bool {
    (raw.len() == 40 || raw.len() == 64)
        && raw
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Validate a strictly lowercase 40-char or 64-char Git OID.
pub fn validate_full_oid(raw: &str, kind: &'static str) -> Result<(), IdError> {
    if is_valid_full_oid(raw) {
        Ok(())
    } else {
        Err(IdError::new(format!(
            "{kind} '{raw}' must be a strictly lowercase 40- or 64-character hexadecimal Git OID"
        )))
    }
}

/// Return whether `raw` is an acceptable CLI short Git hash prefix (`7..=64` ASCII hex digits).
pub fn is_valid_short_hex_prefix(raw: &str) -> bool {
    (7..=64).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_hexdigit())
}

macro_rules! define_ident_newtype {
    ($(#[$meta:meta])* $name:ident, $kind:expr) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
        )]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct and validate a new identifier.
            pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
                let s = raw.into();
                validate_standard_ident(&s, $kind)?;
                Ok(Self(s))
            }

            /// Return the validated identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(de::Error::custom)
            }
        }
    };
}

define_ident_newtype!(
    /// Immutable unique identifier for an execution `Run`.
    RunId,
    "run_id"
);
define_ident_newtype!(
    /// Human-readable unique name for an execution `Run` within a workspace.
    RunName,
    "run_name"
);
define_ident_newtype!(
    /// Validated reference identifier for a task inside a `Run`.
    TaskRef,
    "task_ref"
);

impl TaskRef {
    /// Internal sentinel representing a whole-spec external dependency (`<spec_id>` or `<ws>/<spec_id>`).
    pub const WHOLE_SPEC: &'static str = "*";

    /// Construct the whole-spec dependency sentinel `TaskRef`.
    pub fn whole_spec() -> Self {
        Self(Self::WHOLE_SPEC.to_owned())
    }

    /// Return `true` if this `TaskRef` represents a whole-spec dependency rather than a specific task.
    pub fn is_whole_spec(&self) -> bool {
        self.0 == Self::WHOLE_SPEC
    }
}
define_ident_newtype!(
    /// Validated reference identifier for a subtask inside a task.
    SubtaskRef,
    "subtask_ref"
);
define_ident_newtype!(
    /// Validated worker identity used for claims and independence checks.
    WorkerId,
    "worker_id"
);

/// Validated gate identifier forbidding reserved keywords (`tasks`, `__task__`, `__run__`, `__*`) and path/cursor separators.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct GateId(String);

impl GateId {
    /// Construct and validate a `GateId`.
    pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
        let s = raw.into();
        if s.chars().any(char::is_whitespace) || s.contains(['.', '/', ':']) {
            return Err(IdError::new(format!(
                "gate_id '{s}' must not contain '.', '/', ':', or whitespace"
            )));
        }
        if s == "tasks" || s == "__task__" || s == "__run__" || s.starts_with("__") {
            return Err(IdError::new(format!(
                "gate_id '{s}' uses a reserved identifier ('tasks' or '__*' prefix)"
            )));
        }
        validate_standard_ident(&s, "gate_id")?;
        Ok(Self(s))
    }

    /// Return the validated gate identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for GateId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for GateId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for GateId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for GateId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(de::Error::custom)
    }
}

macro_rules! define_oid_newtype {
    ($(#[$meta:meta])* $name:ident, $kind:expr) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
        )]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct and validate a full lowercase 40- or 64-hex Git OID.
            pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
                let s = raw.into();
                validate_full_oid(&s, $kind)?;
                Ok(Self(s))
            }

            /// Return the full lowercase hexadecimal OID.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Return the 7-character abbreviated prefix for human-facing inspection commands.
            pub fn short(&self) -> &str {
                &self.0[..7]
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(de::Error::custom)
            }
        }
    };
}

define_oid_newtype!(
    /// Full canonical lowercase SHA-1 (40 hex) or SHA-256 (64 hex) Git tree object ID.
    TreeOid,
    "tree_oid"
);
define_oid_newtype!(
    /// Full canonical lowercase SHA-1 (40 hex) or SHA-256 (64 hex) Git commit object ID.
    CommitOid,
    "commit_oid"
);

fn parse_gate_segments(raw: &str, context: &'static str) -> Result<Vec<GateId>, IdError> {
    if raw.is_empty() {
        return Err(IdError::new(format!("{context} cannot be empty")));
    }
    raw.split('.').map(GateId::new).collect()
}

fn join_gates(gates: &[GateId]) -> String {
    let mut out = String::new();
    for (idx, gate) in gates.iter().enumerate() {
        if idx > 0 {
            out.push('.');
        }
        out.push_str(gate.as_str());
    }
    out
}

/// Uniquely addresses a single gate node or task container in a `Run` graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema)]
pub enum NodeCursor {
    /// A root-level gate or container path (`<g1>.<g2>`).
    Root(Vec<GateId>),
    /// A task container (`<task_ref>`) or task gate path (`<task_ref>.<g1>.<g2>`).
    Task {
        /// Owning task reference.
        task: TaskRef,
        /// Nested gate path inside the task container (empty targets the task node itself).
        gates: Vec<GateId>,
    },
}

impl NodeCursor {
    /// Construct a validated root `NodeCursor` requiring at least one `GateId`.
    pub fn root(gates: Vec<GateId>) -> Result<Self, IdError> {
        if gates.is_empty() {
            return Err(IdError::new("root NodeCursor requires at least one GateId"));
        }
        Ok(Self::Root(gates))
    }

    /// Construct a task or task-gate `NodeCursor`.
    pub fn task(task: TaskRef, gates: Vec<GateId>) -> Self {
        Self::Task { task, gates }
    }

    /// Encode this cursor into the canonical database `gate_nodes.node_id` representation.
    ///
    /// - `Root([g1, g2])` -> `root:<g1>.<g2>`
    /// - `Task { task, gates: [] }` -> `task:<task_ref>`
    /// - `Task { task, gates: [g1, g2] }` -> `task:<task_ref>:<g1>.<g2>`
    pub fn to_node_id(&self) -> Result<String, IdError> {
        match self {
            Self::Root(gates) => {
                if gates.is_empty() {
                    return Err(IdError::new("root NodeCursor requires at least one GateId"));
                }
                Ok(format!("root:{}", join_gates(gates)))
            }
            Self::Task { task, gates } => {
                if gates.is_empty() {
                    Ok(format!("task:{task}"))
                } else {
                    Ok(format!("task:{task}:{}", join_gates(gates)))
                }
            }
        }
    }

    /// Decode a canonical database `gate_nodes.node_id` string into a `NodeCursor`.
    pub fn from_node_id(raw: &str) -> Result<Self, IdError> {
        if let Some(rest) = raw.strip_prefix("root:") {
            return Self::root(parse_gate_segments(rest, "root node_id")?);
        }
        if let Some(rest) = raw.strip_prefix("task:") {
            return match rest.split_once(':') {
                Some((task_raw, gates_raw)) => Ok(Self::Task {
                    task: TaskRef::new(task_raw)?,
                    gates: parse_gate_segments(gates_raw, "task gate node_id")?,
                }),
                None => Ok(Self::Task {
                    task: TaskRef::new(rest)?,
                    gates: Vec::new(),
                }),
            };
        }
        Err(IdError::new(format!(
            "invalid gate_nodes.node_id '{raw}'; expected 'root:<gates>' or 'task:<task_ref>[:<gates>]'"
        )))
    }

    /// Encode a `- tasks:` container block index (`tasks#<idx>`).
    pub fn tasks_block_node_id(idx: usize) -> String {
        format!("tasks#{idx}")
    }

    /// Decode a `- tasks:` container block node ID (`tasks#<idx>`).
    pub fn parse_tasks_block_node_id(raw: &str) -> Option<usize> {
        raw.strip_prefix("tasks#")
            .and_then(|rest| rest.parse::<usize>().ok())
    }

    /// Parse a user/CLI cursor string using known task references to disambiguate `<task_ref>[.<gate>]` vs root gates.
    pub fn parse_with_task_lookup(
        raw: &str,
        is_task: impl Fn(&str) -> bool,
    ) -> Result<Self, IdError> {
        if raw.starts_with("root:") || raw.starts_with("task:") {
            return Self::from_node_id(raw);
        }
        if raw.is_empty() {
            return Err(IdError::new("NodeCursor cannot be empty"));
        }
        let mut parts = raw.split('.');
        let first = parts
            .next()
            .ok_or_else(|| IdError::new("NodeCursor cannot be empty"))?;
        let rest: Vec<&str> = parts.collect();
        if is_task(first) {
            let task = TaskRef::new(first)?;
            let gates = rest
                .into_iter()
                .map(GateId::new)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self::Task { task, gates })
        } else {
            Self::root(parse_gate_segments(raw, "NodeCursor")?)
        }
    }
}

impl fmt::Display for NodeCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root(gates) => f.write_str(&join_gates(gates)),
            Self::Task { task, gates } => {
                if gates.is_empty() {
                    write!(f, "{task}")
                } else {
                    write!(f, "{task}.{}", join_gates(gates))
                }
            }
        }
    }
}

impl FromStr for NodeCursor {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("root:") || s.starts_with("task:") {
            return Self::from_node_id(s);
        }
        Self::root(parse_gate_segments(s, "NodeCursor")?)
    }
}

impl<'de> Deserialize<'de> for NodeCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TaskCursorHelper {
            task: TaskRef,
            #[serde(default)]
            gates: Vec<GateId>,
        }

        struct NodeCursorVisitor;

        impl<'de> Visitor<'de> for NodeCursorVisitor {
            type Value = NodeCursor;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a NodeCursor string or tagged enum {Root: [...]} / {Task: {...}}")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                NodeCursor::from_str(v).map_err(de::Error::custom)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let Some(key) = map.next_key::<String>()? else {
                    return Err(de::Error::custom("empty map for NodeCursor"));
                };
                let cursor = match key.as_str() {
                    "Root" => {
                        let gates: Vec<GateId> = map.next_value()?;
                        NodeCursor::root(gates).map_err(de::Error::custom)?
                    }
                    "Task" => {
                        let helper: TaskCursorHelper = map.next_value()?;
                        NodeCursor::Task {
                            task: helper.task,
                            gates: helper.gates,
                        }
                    }
                    other => {
                        return Err(de::Error::unknown_variant(other, &["Root", "Task"]));
                    }
                };
                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom("unexpected extra key in NodeCursor"));
                }
                Ok(cursor)
            }
        }

        deserializer.deserialize_any(NodeCursorVisitor)
    }
}

/// Gate reference selector used in `independent_from` and `gates_patch:` (`polish` or `"tasks.build"`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateSelector {
    /// Local or root gate path (`polish` or `blueprint_synthesis.review`).
    Local(Vec<GateId>),
    /// Cross-task gate selector targeting a gate across all non-cancelled tasks (`tasks.build`).
    AllTasksGate(Vec<GateId>),
}

impl GateSelector {
    /// Construct a validated local `GateSelector` requiring at least one `GateId`.
    pub fn local(gates: Vec<GateId>) -> Result<Self, IdError> {
        if gates.is_empty() {
            return Err(IdError::new(
                "local GateSelector requires at least one GateId",
            ));
        }
        Ok(Self::Local(gates))
    }

    /// Construct a validated `AllTasksGate` selector requiring at least one `GateId`.
    pub fn all_tasks_gate(gates: Vec<GateId>) -> Result<Self, IdError> {
        if gates.is_empty() {
            return Err(IdError::new(
                "AllTasksGate selector requires at least one GateId after 'tasks.'",
            ));
        }
        Ok(Self::AllTasksGate(gates))
    }
}

impl fmt::Display for GateSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(gates) => f.write_str(&join_gates(gates)),
            Self::AllTasksGate(gates) => write!(f, "tasks.{}", join_gates(gates)),
        }
    }
}

impl FromStr for GateSelector {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("tasks.") {
            return Self::all_tasks_gate(parse_gate_segments(rest, "AllTasksGate selector")?);
        }
        if s == "tasks" {
            return Err(IdError::new(
                "GateSelector 'tasks' must specify a child gate (e.g. 'tasks.build')",
            ));
        }
        Self::local(parse_gate_segments(s, "Local GateSelector")?)
    }
}

impl Serialize for GateSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GateSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(de::Error::custom)
    }
}

impl schemars::JsonSchema for GateSelector {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("GateSelector")
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_identifiers_validate_on_fromstr_and_deserialize() {
        let valid_cases = ["a", "A9", "AC-008", "worker_1", &"x".repeat(64)];
        let invalid_cases = [
            "",
            "-abc",
            "_abc",
            "has space",
            "has.dot",
            "has/slash",
            "has:colon",
            &"x".repeat(65),
        ];

        for raw in valid_cases {
            assert_eq!(RunId::from_str(raw).unwrap().as_str(), raw);
            assert_eq!(RunName::from_str(raw).unwrap().as_str(), raw);
            assert_eq!(TaskRef::from_str(raw).unwrap().as_str(), raw);
            assert_eq!(SubtaskRef::from_str(raw).unwrap().as_str(), raw);
            assert_eq!(WorkerId::from_str(raw).unwrap().as_str(), raw);

            let json = format!("\"{raw}\"");
            assert_eq!(serde_json::from_str::<RunId>(&json).unwrap().as_str(), raw);
            assert_eq!(
                serde_json::from_str::<TaskRef>(&json).unwrap().as_str(),
                raw
            );
            assert_eq!(
                serde_json::from_str::<WorkerId>(&json).unwrap().as_str(),
                raw
            );
        }

        for raw in invalid_cases {
            assert!(RunId::from_str(raw).is_err(), "expected RunId err for {raw:?}");
            assert!(RunName::from_str(raw).is_err(), "expected RunName err for {raw:?}");
            assert!(TaskRef::from_str(raw).is_err(), "expected TaskRef err for {raw:?}");
            assert!(
                SubtaskRef::from_str(raw).is_err(),
                "expected SubtaskRef err for {raw:?}"
            );
            assert!(
                WorkerId::from_str(raw).is_err(),
                "expected WorkerId err for {raw:?}"
            );
            let json = serde_json::to_string(raw).unwrap();
            assert!(serde_json::from_str::<RunId>(&json).is_err());
        }
    }

    #[test]
    fn gate_id_rejects_reserved_words_and_separators() {
        let cases: &[(&str, bool)] = &[
            ("build", true),
            ("blueprint_synthesis", true),
            ("contract-review_1", true),
            ("tasks", false),
            ("__task__", false),
            ("__run__", false),
            ("__custom", false),
            ("a.b", false),
            ("a/b", false),
            ("a:b", false),
            ("a b", false),
            ("", false),
            ("-bad", false),
        ];

        for &(raw, expect_ok) in cases {
            assert_eq!(
                GateId::from_str(raw).is_ok(),
                expect_ok,
                "GateId::from_str({raw:?})"
            );
            let json = serde_json::to_string(raw).unwrap();
            assert_eq!(
                serde_json::from_str::<GateId>(&json).is_ok(),
                expect_ok,
                "GateId deserialize({raw:?})"
            );
        }
    }

    #[test]
    fn git_oids_enforce_strict_lowercase_hex_40_or_64() {
        let sha1_lower = "a".repeat(40);
        let sha256_lower = "0123456789abcdef".repeat(4);
        let sha1_upper = "A".repeat(40);
        let cases: &[(&str, bool, bool)] = &[
            (&sha1_lower, true, true),
            (&sha256_lower, true, true),
            (&sha1_upper, false, true),
            ("0123456", false, true),
            ("012345", false, false),
            ("0123456789abcdef0123456789abcdef0123456g", false, false),
        ];

        for &(raw, expect_full, expect_prefix) in cases {
            assert_eq!(is_valid_full_oid(raw), expect_full, "full oid {raw:?}");
            assert_eq!(
                TreeOid::from_str(raw).is_ok(),
                expect_full,
                "TreeOid({raw:?})"
            );
            assert_eq!(
                CommitOid::from_str(raw).is_ok(),
                expect_full,
                "CommitOid({raw:?})"
            );
            let json = serde_json::to_string(raw).unwrap();
            assert_eq!(serde_json::from_str::<TreeOid>(&json).is_ok(), expect_full);
            assert_eq!(
                is_valid_short_hex_prefix(raw),
                expect_prefix,
                "short prefix {raw:?}"
            );
        }
        assert_eq!(TreeOid::new(&sha1_lower).unwrap().short(), "aaaaaaa");
    }

    #[test]
    fn node_cursor_display_fromstr_node_id_and_serde() {
        let g = |s: &str| GateId::new(s).unwrap();
        let t = |s: &str| TaskRef::new(s).unwrap();

        let cases = [
            (
                NodeCursor::root(vec![g("blueprint_synthesis"), g("review")]).unwrap(),
                "blueprint_synthesis.review",
                "root:blueprint_synthesis.review",
            ),
            (NodeCursor::task(t("T1"), vec![]), "T1", "task:T1"),
            (
                NodeCursor::task(t("T1"), vec![g("build")]),
                "T1.build",
                "task:T1:build",
            ),
            (
                NodeCursor::task(t("T1"), vec![g("seam"), g("review")]),
                "T1.seam.review",
                "task:T1:seam.review",
            ),
        ];

        for (cursor, expected_display, expected_node_id) in cases {
            assert_eq!(cursor.to_string(), expected_display);
            assert_eq!(cursor.to_node_id().unwrap(), expected_node_id);
            assert_eq!(NodeCursor::from_node_id(expected_node_id).unwrap(), cursor);
            assert_eq!(NodeCursor::from_str(expected_node_id).unwrap(), cursor);

            let serialized = serde_json::to_string(&cursor).unwrap();
            let roundtrip: NodeCursor = serde_json::from_str(&serialized).unwrap();
            assert_eq!(roundtrip, cursor);
        }

        // Context-aware parse disambiguates task refs from root gates
        assert_eq!(
            NodeCursor::parse_with_task_lookup("T1.build", |seg| seg == "T1").unwrap(),
            NodeCursor::task(t("T1"), vec![g("build")])
        );
        assert_eq!(
            NodeCursor::parse_with_task_lookup("blueprint_synthesis.review", |seg| seg == "T1")
                .unwrap(),
            NodeCursor::root(vec![g("blueprint_synthesis"), g("review")]).unwrap()
        );

        // tasks#idx helpers & invalid node_ids
        assert_eq!(NodeCursor::tasks_block_node_id(0), "tasks#0");
        assert_eq!(NodeCursor::parse_tasks_block_node_id("tasks#2"), Some(2));
        assert!(NodeCursor::from_node_id("tasks#0").is_err());
        assert!(NodeCursor::from_str("tasks.build").is_err());
        assert!(serde_json::from_str::<NodeCursor>(r#"{"Root":[]}"#).is_err());
    }

    #[test]
    fn gate_selector_roundtrip_and_validation() {
        let g = |s: &str| GateId::new(s).unwrap();
        let cases: &[(&str, Option<GateSelector>)] = &[
            ("polish", Some(GateSelector::Local(vec![g("polish")]))),
            (
                "blueprint_synthesis.review",
                Some(GateSelector::Local(vec![
                    g("blueprint_synthesis"),
                    g("review"),
                ])),
            ),
            (
                "tasks.build",
                Some(GateSelector::AllTasksGate(vec![g("build")])),
            ),
            (
                "tasks.seam.review",
                Some(GateSelector::AllTasksGate(vec![g("seam"), g("review")])),
            ),
            ("tasks", None),
            ("tasks.", None),
            ("tasks.tasks", None),
            ("", None),
            (".polish", None),
        ];

        for (raw, expected) in cases {
            let parsed = GateSelector::from_str(raw);
            match expected {
                Some(sel) => {
                    assert_eq!(&parsed.unwrap(), sel);
                    assert_eq!(sel.to_string(), *raw);
                    let json = serde_json::to_string(sel).unwrap();
                    assert_eq!(json, format!("\"{raw}\""));
                    assert_eq!(&serde_json::from_str::<GateSelector>(&json).unwrap(), sel);
                }
                None => {
                    assert!(parsed.is_err(), "expected err for {raw:?}");
                    let json = serde_json::to_string(raw).unwrap();
                    assert!(serde_json::from_str::<GateSelector>(&json).is_err());
                }
            }
        }
    }
}
