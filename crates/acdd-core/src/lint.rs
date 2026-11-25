//! Pure leaf architectural, graph hygiene, and documentation lint contracts.

use crate::errors::{ErrorCode, StateError};
use crate::path_policy::{glob_matches, is_valid_scope_glob, normalize_rel_path};
use crate::schema::{AdditionalPropertiesDef, SchemaDef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

fn schema_invalid(msg: impl Into<String>) -> StateError {
    StateError::new(ErrorCode::ProfileSchemaInvalid, msg.into())
}

macro_rules! closed_lint_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        $vis enum $name { $($(#[$vmeta])* #[serde(rename = $wire)] $variant),+ }

        impl $name {
            /// All closed variants of this enum in canonical declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            /// Canonical `snake_case` wire identifier.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }

        impl FromStr for $name {
            type Err = StateError;
            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                match raw {
                    $($wire => Ok(Self::$variant),)+
                    other => Err(schema_invalid(format!(
                        "unknown {} '{other}'; expected one of: {}",
                        stringify!($name),
                        Self::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                    ))),
                }
            }
        }
    };
}

closed_lint_enum! {
    /// Closed set of lint categories evaluated by the unified lint engine.
    pub enum LintCategory {
        Arch => "arch",
        Duplicates => "duplicates",
        DeadTails => "dead_tails",
        Cycles => "cycles",
        Syntax => "syntax",
        Docs => "docs",
    }
}

impl LintCategory {
    /// Default verification categories enabled when callers omit an explicit category filter.
    pub const DEFAULT_VERIFY: [Self; 3] = [Self::Arch, Self::Syntax, Self::Docs];
}

closed_lint_enum! {
    /// Closed diagnostic severity level ordered from lowest (`warn`) to highest (`error`).
    #[derive(Default)]
    pub enum LintSeverity {
        Warn => "warn",
        #[default]
        Error => "error",
    }
}

impl LintSeverity {
    /// Return `true` when `self` is at least as severe as `minimum`.
    pub const fn meets_threshold(self, minimum: Self) -> bool {
        (self as u8) >= (minimum as u8)
    }
}

closed_lint_enum! {
    /// Closed set of 8 language-agnostic architectural rule primitives.
    pub enum ArchRuleKind {
        ForbidDependency => "forbid_dependency",
        AllowOnlyDependencies => "allow_only_dependencies",
        SingleOwner => "single_owner",
        ForbidCycles => "forbid_cycles",
        BoundaryEntry => "boundary_entry",
        MustImplement => "must_implement",
        DocIntegrity => "doc_integrity",
        AstPattern => "ast_pattern",
    }
}

closed_lint_enum! {
    /// Closed graph edge kind selector matching `crates/acdd-index` `EdgeKind` wire names.
    pub enum LintEdgeKind {
        Calls => "calls",
        PossibleCalls => "possible_calls",
        Imports => "imports",
        Contains => "contains",
        References => "references",
        Inherits => "inherits",
        Implements => "implements",
        Bases => "bases",
        Decorates => "decorates",
        Handles => "handles",
        Mutates => "mutates",
        Registers => "registers",
        Governs => "governs",
        Documents => "documents",
        ReferencesDoc => "references_doc",
        Overrides => "overrides",
        Extends => "extends",
    }
}

closed_lint_enum! {
    /// Closed edge confidence selector matching `crates/acdd-index` `EdgeConfidence` wire names.
    pub enum LintEdgeConfidence {
        Exact => "exact",
        Inferred => "inferred",
        Possible => "possible",
        Heuristic => "heuristic",
    }
}

/// Language-agnostic selector matching zones, path globs, symbols, external prefixes, decorators, or languages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneSelector {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_symbols: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
}

impl ZoneSelector {
    /// Return `true` when no positive selector criteria are configured.
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
            && self.paths.is_empty()
            && self.symbols.is_empty()
            && self.external_symbols.is_empty()
            && self.decorators.is_empty()
            && self.languages.is_empty()
    }

    /// Return `true` when this selector constrains files via `zones` or `paths`.
    pub fn has_path_scope(&self) -> bool {
        !self.zones.is_empty() || !self.paths.is_empty()
    }

    /// Return `true` when `rel_path` is invalid or matches any glob in `exclude_paths`.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return true;
        };
        self.exclude_paths
            .iter()
            .any(|pat| glob_matches(pat, &norm))
    }

    /// Return `true` when `rel_path` matches a `paths` or referenced `zones` glob and is not excluded.
    pub fn matches_path(&self, rel_path: &str, zones: &BTreeMap<String, Vec<String>>) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        if self
            .exclude_paths
            .iter()
            .any(|pat| glob_matches(pat, &norm))
        {
            return false;
        }
        if self.paths.iter().any(|pat| glob_matches(pat, &norm)) {
            return true;
        }
        self.zones.iter().any(|zone_name| {
            zones
                .get(zone_name)
                .is_some_and(|globs| globs.iter().any(|pat| glob_matches(pat, &norm)))
        })
    }

    /// Validate all selector fields, glob syntax, and optional zone references against `known_zones`.
    pub fn validate(
        &self,
        known_zones: Option<&BTreeSet<String>>,
        ctx: &str,
    ) -> Result<(), StateError> {
        if self.is_empty() {
            return Err(schema_invalid(format!("{ctx} selector must not be empty")));
        }
        validate_zone_refs(&self.zones, known_zones, &format!("{ctx}.zones"))?;
        validate_scope_globs(&self.paths, &format!("{ctx}.paths"))?;
        validate_scope_globs(&self.exclude_paths, &format!("{ctx}.exclude_paths"))?;
        validate_non_empty_strings(&self.symbols, &format!("{ctx}.symbols"))?;
        validate_non_empty_strings(&self.external_symbols, &format!("{ctx}.external_symbols"))?;
        validate_non_empty_strings(&self.decorators, &format!("{ctx}.decorators"))?;
        validate_non_empty_strings(&self.languages, &format!("{ctx}.languages"))?;
        Ok(())
    }
}

/// Executable architectural rule specification parsed from `acdd.yaml` or fenced `acdd-rule` blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchRuleSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invariant: Option<String>,
    #[serde(default)]
    pub severity: LintSeverity,
    pub kind: ArchRuleKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ZoneSelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<ZoneSelector>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_kinds: Vec<LintEdgeKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confidences: Vec<LintEdgeConfidence>,
    #[serde(default)]
    pub include_tests: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ast_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ArchRuleSpec {
    /// Validate rule fields, selector non-emptiness, glob syntax, and optional zone references.
    pub fn validate(&self, known_zones: Option<&BTreeSet<String>>) -> Result<(), StateError> {
        if self.id.trim().is_empty() {
            return Err(schema_invalid("arch rule 'id' must not be empty"));
        }
        let rule_ctx = format!("rule '{}'", self.id);
        for (field, opt) in [("invariant", &self.invariant), ("message", &self.message)] {
            if opt.as_deref().is_some_and(|v| v.trim().is_empty()) {
                return Err(schema_invalid(format!(
                    "{rule_ctx}.{field} must not be empty when specified"
                )));
            }
        }
        if let Some(from) = &self.from {
            from.validate(known_zones, &format!("{rule_ctx}.from"))?;
        }
        if let Some(to) = &self.to {
            to.validate(known_zones, &format!("{rule_ctx}.to"))?;
        }
        validate_scope_globs(&self.owner_paths, &format!("{rule_ctx}.owner_paths"))?;
        validate_scope_globs(&self.entrypoints, &format!("{rule_ctx}.entrypoints"))?;
        validate_zone_refs(&self.zones, known_zones, &format!("{rule_ctx}.zones"))?;
        if self.depth == Some(0) {
            return Err(schema_invalid(format!(
                "{rule_ctx}.depth must be at least 1"
            )));
        }
        match self.kind {
            ArchRuleKind::ForbidDependency
            | ArchRuleKind::AllowOnlyDependencies
            | ArchRuleKind::MustImplement => {
                if self.from.is_none() || self.to.is_none() {
                    return Err(schema_invalid(format!(
                        "{rule_ctx} ({}) requires both 'from' and 'to' selectors",
                        self.kind
                    )));
                }
            }
            ArchRuleKind::SingleOwner => {
                if self.owner_paths.is_empty() || self.to.is_none() {
                    return Err(schema_invalid(format!("{rule_ctx} (single_owner) requires non-empty 'owner_paths' and 'to' selector")));
                }
            }
            ArchRuleKind::BoundaryEntry => {
                if self.to.is_none() || self.entrypoints.is_empty() {
                    return Err(schema_invalid(format!("{rule_ctx} (boundary_entry) requires 'to' selector and non-empty 'entrypoints'")));
                }
            }
            ArchRuleKind::AstPattern => {
                if self.from.is_none()
                    || self
                        .ast_pattern
                        .as_deref()
                        .is_none_or(|p| p.trim().is_empty())
                {
                    return Err(schema_invalid(format!("{rule_ctx} (ast_pattern) requires 'from' selector and non-empty 'ast_pattern'")));
                }
            }
            ArchRuleKind::ForbidCycles | ArchRuleKind::DocIntegrity => {}
        }
        Ok(())
    }
}

/// Syntax invariant settings evaluated under `LintCategory::Syntax` (`syntax.fact_budget_exceeded` and `syntax.intra_crate_type_homonym`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ComplexityLimits {
    /// Diagnostic severity emitted for syntax invariant violations (such as `syntax.fact_budget_exceeded`).
    pub severity: LintSeverity,
    /// Optional invariant ID bound to syntax checks in `enforced_invariant_ids`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invariant: Option<String>,
}

impl Default for ComplexityLimits {
    fn default() -> Self {
        Self {
            severity: LintSeverity::Warn,
            invariant: None,
        }
    }
}

impl ComplexityLimits {
    /// Return `true` when every field matches [`ComplexityLimits::default`].
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Validate that any complexity invariant binding is non-empty.
    pub fn validate(&self) -> Result<(), StateError> {
        if self
            .invariant
            .as_deref()
            .is_some_and(|v| v.trim().is_empty())
        {
            return Err(schema_invalid(
                "architecture.complexity.invariant must not be empty when specified",
            ));
        }
        Ok(())
    }
}

/// Optional `architecture:` configuration block in `acdd.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureConfig {
    /// Named architectural zones mapped to workspace-relative path globs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub zones: BTreeMap<String, Vec<String>>,
    /// Static architectural rules declared directly in `acdd.yaml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ArchRuleSpec>,
    /// Structural complexity thresholds evaluated under `LintCategory::Syntax`.
    #[serde(default, skip_serializing_if = "ComplexityLimits::is_default")]
    pub complexity: ComplexityLimits,
    /// Opt-in flag enabling `dead_tails.unused_public_api` checks (disabled by default for libraries).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unused_public_api: bool,
}

impl ArchitectureConfig {
    /// Return `true` when no zones, rules, custom complexity thresholds, or opt-in flags are configured.
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
            && self.rules.is_empty()
            && self.complexity.is_default()
            && !self.unused_public_api
    }

    /// Return the deterministic set of declared zone names.
    pub fn zone_names(&self) -> BTreeSet<String> {
        self.zones.keys().cloned().collect()
    }

    /// Validate all declared zones, glob patterns, complexity limits, rule ID uniqueness, and zone references.
    pub fn validate(&self) -> Result<(), StateError> {
        self.complexity.validate()?;
        for (zone_name, globs) in &self.zones {
            if zone_name.trim().is_empty() {
                return Err(schema_invalid("architecture.zones name must not be empty"));
            }
            if globs.is_empty() {
                return Err(schema_invalid(format!(
                    "architecture.zones.{zone_name} must contain at least one glob pattern"
                )));
            }
            validate_scope_globs(globs, &format!("architecture.zones.{zone_name}"))?;
        }
        let known_zones = self.zone_names();
        let mut seen_ids = BTreeSet::new();
        for rule in &self.rules {
            if !seen_ids.insert(rule.id.as_str()) {
                return Err(schema_invalid(format!(
                    "duplicate architecture.rules id '{}'",
                    rule.id
                )));
            }
            rule.validate(Some(&known_zones))?;
        }
        Ok(())
    }
}

fn normalize_external_symbol_segments(symbol: &str) -> Vec<&str> {
    symbol
        .trim()
        .split([':', '.', '/'])
        .filter(|seg| !seg.is_empty())
        .collect()
}

/// Return `true` when `symbol` matches `prefix` on normalized `::`, `.`, `/`, and `:` segment boundaries.
pub fn external_symbol_matches_prefix(symbol: &str, prefix: &str) -> bool {
    let pre = normalize_external_symbol_segments(prefix);
    if pre.is_empty() {
        return false;
    }
    let sym = normalize_external_symbol_segments(symbol);
    sym.len() >= pre.len() && sym[..pre.len()] == pre[..]
}

/// Canonical `SchemaDef` for the `code.lint` gate check input parameters.
pub fn lint_check_input_schema() -> SchemaDef {
    let mut category_item = SchemaDef::string();
    category_item.enum_values = Some(
        LintCategory::ALL
            .iter()
            .map(|c| Value::String(c.as_str().to_owned()))
            .collect(),
    );
    let mut severity_schema = SchemaDef::string();
    severity_schema.enum_values = Some(
        LintSeverity::ALL
            .iter()
            .map(|s| Value::String(s.as_str().to_owned()))
            .collect(),
    );
    let mut non_empty_str = SchemaDef::string();
    non_empty_str.min_length = Some(1);

    let mut root = SchemaDef::object();
    root.properties
        .insert("categories".to_owned(), SchemaDef::array(category_item));
    root.properties
        .insert("severity".to_owned(), severity_schema);
    root.properties
        .insert("rules".to_owned(), SchemaDef::array(non_empty_str.clone()));
    root.properties.insert("target".to_owned(), non_empty_str);
    root.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
    root
}

fn validate_non_empty_strings(items: &[String], ctx: &str) -> Result<(), StateError> {
    for item in items {
        if item.trim().is_empty() {
            return Err(schema_invalid(format!("{ctx} entries must not be empty")));
        }
    }
    Ok(())
}

fn validate_scope_globs(globs: &[String], ctx: &str) -> Result<(), StateError> {
    for glob in globs {
        if !is_valid_scope_glob(glob) {
            return Err(schema_invalid(format!(
                "{ctx} contains invalid workspace-relative glob '{glob}'"
            )));
        }
    }
    Ok(())
}

fn validate_zone_refs(
    zones: &[String],
    known_zones: Option<&BTreeSet<String>>,
    ctx: &str,
) -> Result<(), StateError> {
    for zone in zones {
        if zone.trim().is_empty() {
            return Err(schema_invalid(format!("{ctx} entries must not be empty")));
        }
        if known_zones.is_some_and(|known| !known.contains(zone)) {
            return Err(schema_invalid(format!(
                "{ctx} references unknown zone '{zone}'"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lint_enums_roundtrip_and_reject_unknown_values() {
        assert_eq!(LintCategory::ALL.len(), 6);
        assert_eq!(
            LintCategory::DEFAULT_VERIFY,
            [LintCategory::Arch, LintCategory::Syntax, LintCategory::Docs]
        );
        for &cat in LintCategory::ALL {
            assert_eq!(cat.to_string(), cat.as_str());
            assert_eq!(cat.as_str().parse::<LintCategory>().unwrap(), cat);
            assert_eq!(
                serde_json::from_value::<LintCategory>(json!(cat.as_str())).unwrap(),
                cat
            );
        }
        assert_eq!(
            "bogus".parse::<LintCategory>().unwrap_err().code,
            ErrorCode::ProfileSchemaInvalid
        );

        assert_eq!(LintSeverity::default(), LintSeverity::Error);
        assert!(LintSeverity::Error.meets_threshold(LintSeverity::Warn));
        assert!(!LintSeverity::Warn.meets_threshold(LintSeverity::Error));
        for &sev in LintSeverity::ALL {
            assert_eq!(sev.as_str().parse::<LintSeverity>().unwrap(), sev);
        }
        assert_eq!(ArchRuleKind::ALL.len(), 8);
        for &k in ArchRuleKind::ALL {
            assert_eq!(k.as_str().parse::<ArchRuleKind>().unwrap(), k);
        }
        assert_eq!(LintEdgeKind::ALL.len(), 17);
        for &ek in LintEdgeKind::ALL {
            assert_eq!(ek.as_str().parse::<LintEdgeKind>().unwrap(), ek);
        }
        assert_eq!(LintEdgeConfidence::ALL.len(), 4);
        for &ec in LintEdgeConfidence::ALL {
            assert_eq!(ec.as_str().parse::<LintEdgeConfidence>().unwrap(), ec);
        }
    }

    #[test]
    fn arch_rule_spec_and_architecture_config_validation_matrix() {
        let valid_yaml = r#"
zones:
  core: ["crates/acdd-core/src/**"]
  index: ["crates/acdd-index/src/**"]
rules:
  - id: INV-CORE-LEAF
    invariant: INV-CORE-LEAF
    severity: error
    kind: forbid_dependency
    from: { zones: ["core"] }
    to: { zones: ["index"], external_symbols: ["rusqlite"] }
    edge_kinds: ["imports", "calls", "references"]
    confidences: ["exact", "inferred"]
  - id: INV-ALLOW-ONLY
    kind: allow_only_dependencies
    from: { paths: ["src/{cli,mcp}/**"] }
    to: { zones: ["core"] }
  - id: INV-GIT-OWNER
    kind: single_owner
    owner_paths: ["crates/acdd-core/src/git.rs"]
    to: { external_symbols: ["std::process::Command"] }
  - id: INV-NO-ZONES-CYCLE
    kind: forbid_cycles
    zones: ["core", "index"]
    depth: 2
  - id: INV-BOUNDARY
    kind: boundary_entry
    to: { zones: ["index"] }
    entrypoints: ["**/mod.rs", "**/lib.rs"]
  - id: INV-TRAIT
    kind: must_implement
    from: { paths: ["src/providers/**"], decorators: ["provider"], languages: ["python"] }
    to: { symbols: ["DomainProvider"] }
  - id: INV-DOCS
    kind: doc_integrity
    severity: warn
    from: { paths: ["docs/**"] }
  - id: INV-AST
    kind: ast_pattern
    from: { paths: ["src/**", "crates/**"], exclude_paths: ["crates/acdd-core/src/git.rs"], languages: ["rust"] }
    ast_pattern: 'Command::new("git")'
"#;
        let cfg: ArchitectureConfig = serde_yaml::from_str(valid_yaml).unwrap();
        assert!(!cfg.is_empty());
        assert_eq!(
            cfg.zone_names(),
            BTreeSet::from(["core".to_owned(), "index".to_owned()])
        );
        cfg.validate().unwrap();

        let bad_configs: &[(&str, &str)] = &[
            ("unknown_top_level_field", "zones: {}\nunknown_field: true\n"),
            ("unknown_selector_field", "rules:\n  - id: r1\n    kind: doc_integrity\n    from: { paths: [\"docs/**\"], bogus: 1 }\n"),
            ("empty_selector", "rules:\n  - id: r1\n    kind: doc_integrity\n    from: {}\n"),
            ("exclude_only_selector", "rules:\n  - id: r1\n    kind: doc_integrity\n    from: { exclude_paths: [\"docs/archive/**\"] }\n"),
            ("unknown_zone_ref", "zones:\n  core: [\"crates/acdd-core/**\"]\nrules:\n  - id: r1\n    kind: forbid_dependency\n    from: { zones: [\"missing\"] }\n    to: { zones: [\"core\"] }\n"),
            ("duplicate_rule_id", "rules:\n  - id: dup\n    kind: doc_integrity\n  - id: dup\n    kind: doc_integrity\n"),
            ("invalid_zone_glob", "zones:\n  bad: [\"../outside/**\"]\n"),
            ("empty_zone_globs", "zones:\n  empty: []\n"),
            ("forbid_dep_missing_to", "rules:\n  - id: r1\n    kind: forbid_dependency\n    from: { paths: [\"src/**\"] }\n"),
            ("single_owner_missing_owners", "rules:\n  - id: r1\n    kind: single_owner\n    to: { external_symbols: [\"rusqlite\"] }\n"),
            ("boundary_missing_entrypoints", "rules:\n  - id: r1\n    kind: boundary_entry\n    to: { paths: [\"src/**\"] }\n"),
            ("ast_pattern_missing_pattern", "rules:\n  - id: r1\n    kind: ast_pattern\n    from: { paths: [\"src/**\"] }\n"),
            ("forbid_cycles_zero_depth", "rules:\n  - id: r1\n    kind: forbid_cycles\n    depth: 0\n"),
        ];
        for &(label, yaml) in bad_configs {
            if let Ok(cfg) = serde_yaml::from_str::<ArchitectureConfig>(yaml) {
                let err = cfg
                    .validate()
                    .expect_err(&format!("{label}: expected validation failure"));
                assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{label}: {err}");
            }
        }
    }

    #[test]
    fn zone_selector_matches_paths_and_external_symbol_prefixes() {
        let zones = BTreeMap::from([(
            "core".to_owned(),
            vec!["crates/{acdd-core,acdd-leaf}/src/**".to_owned()],
        )]);
        let sel = ZoneSelector {
            zones: vec!["core".to_owned()],
            paths: vec!["src/shared/*.rs".to_owned()],
            exclude_paths: vec!["crates/acdd-core/src/git.rs".to_owned()],
            ..ZoneSelector::default()
        };
        assert!(sel.has_path_scope());
        assert!(sel.matches_path("crates/acdd-core/src/lib.rs", &zones));
        assert!(sel.matches_path("crates/acdd-leaf/src/mod.rs", &zones));
        assert!(sel.matches_path("src/shared/fmt.rs", &zones));
        assert!(!sel.matches_path("crates/acdd-core/src/git.rs", &zones));
        assert!(!sel.matches_path("crates/acdd-index/src/lib.rs", &zones));
        assert!(!sel.matches_path("../outside.rs", &zones));

        let ext_only = ZoneSelector {
            external_symbols: vec!["rusqlite".to_owned()],
            ..ZoneSelector::default()
        };
        assert!(!ext_only.has_path_scope());
        assert!(!ext_only.matches_path("crates/acdd-core/src/lib.rs", &zones));

        for &(sym, pre, expected) in &[
            ("rusqlite::Connection", "rusqlite", true),
            ("rusqlite.connect", "rusqlite", true),
            ("rusqlite/driver", "rusqlite", true),
            ("rusqlite:v1", "rusqlite", true),
            ("rusqlite", "rusqlite", true),
            ("rusqlite_helper", "rusqlite", false),
            ("rusqlite_helper::open", "rusqlite", false),
            ("std::process::Command::new", "std::process::Command", true),
            ("std.process.Command", "std::process::Command", true),
            ("std::process::CommandArgs", "std::process::Command", false),
            ("anything", "", false),
        ] {
            assert_eq!(
                external_symbol_matches_prefix(sym, pre),
                expected,
                "{sym:?} vs {pre:?}"
            );
        }
    }

    #[test]
    fn lint_check_input_schema_validates_contract() {
        let schema = lint_check_input_schema();
        schema.validate_schema("$").unwrap();
        let _json_schema = schemars::schema_for!(ArchitectureConfig);

        for (label, val, expect_ok) in [
            ("empty", json!({}), true),
            (
                "valid",
                json!({"categories": ["arch", "docs"], "severity": "warn", "rules": ["INV-CORE-LEAF"], "target": "crates/acdd-core/src"}),
                true,
            ),
            ("bad_cat", json!({"categories": ["bogus"]}), false),
            ("bad_sev", json!({"severity": "fatal"}), false),
            ("empty_rule", json!({"rules": [""]}), false),
            ("empty_target", json!({"target": ""}), false),
            ("extra_prop", json!({"extra": true}), false),
        ] {
            assert_eq!(
                schema.validate_value(&val, "$").is_ok(),
                expect_ok,
                "{label}"
            );
        }
    }
}
