//! Typed registries shared by ACDD profiles and workspace adapters.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

/// Typed identifier for built-in ACDD subsystems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemId {
    /// Code indexing, AST, and graph query subsystem (`code`, alias `codemap`).
    Code,
    /// Documentation indexing and lookup subsystem (`docs`).
    Docs,
    /// Task state machine and run lifecycle subsystem (`state`, alias `acdd`).
    State,
    /// Shared run blackboard subsystem (`blackboard`).
    Blackboard,
    /// Reserved memory subsystem (`memory`), inactive unless explicitly enabled.
    Memory,
}

impl SubsystemId {
    /// All canonical built-in subsystem identifiers in stable declaration order.
    pub const ALL: [Self; 5] = [
        Self::Code,
        Self::Docs,
        Self::State,
        Self::Blackboard,
        Self::Memory,
    ];

    /// Canonical lowercase identifier for this subsystem.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Docs => "docs",
            Self::State => "state",
            Self::Blackboard => "blackboard",
            Self::Memory => "memory",
        }
    }

    /// Accepted legacy or short aliases that resolve to this subsystem.
    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Code => &["codemap"],
            Self::State => &["acdd"],
            Self::Docs | Self::Blackboard | Self::Memory => &[],
        }
    }

    /// Return `true` for reserved subsystems that stay off unless explicitly enabled.
    pub const fn is_reserved_default_off(self) -> bool {
        matches!(self, Self::Memory)
    }
}

impl fmt::Display for SubsystemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SubsystemId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "code" | "codemap" => Ok(Self::Code),
            "docs" => Ok(Self::Docs),
            "state" | "acdd" => Ok(Self::State),
            "blackboard" => Ok(Self::Blackboard),
            "memory" => Ok(Self::Memory),
            other => anyhow::bail!("unknown ACDD subsystem '{other}'"),
        }
    }
}

/// Built-in subsystem presets for the MCP tool surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemPreset {
    /// Enable every active built-in subsystem.
    #[default]
    Full,
    /// Enable code navigation and documentation tools.
    Navigator,
    /// Enable task orchestration and blackboard tools.
    Orchestrator,
    /// Enable no module-owned tools; `acdd_guide` remains available as a core tool.
    Minimal,
}

/// Configurable enablement for the ACDD MCP subsystems and external domains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SubsystemRegistry {
    /// Named built-in profile used when no per-subsystem override is present.
    pub profile: SubsystemPreset,
    /// Explicitly enabled subsystem identifiers.
    pub enabled: BTreeSet<String>,
    /// Explicitly disabled subsystem identifiers.
    pub disabled: BTreeSet<String>,
    /// Subsystems required by the active profile.
    pub required: BTreeSet<String>,
    /// Enablement toggles for external `DomainProvider` plugins (`domains.<name>`).
    pub domains: BTreeMap<String, bool>,
}

impl Default for SubsystemRegistry {
    fn default() -> Self {
        Self {
            profile: SubsystemPreset::Full,
            enabled: BTreeSet::new(),
            disabled: BTreeSet::new(),
            required: BTreeSet::new(),
            domains: BTreeMap::new(),
        }
    }
}

fn set_contains_subsystem(set: &BTreeSet<String>, id: SubsystemId) -> bool {
    set.contains(id.as_str()) || id.aliases().iter().any(|alias| set.contains(*alias))
}

impl SubsystemRegistry {
    /// Validate names and reject contradictory or unsatisfied requirements.
    pub fn validate(&self) -> anyhow::Result<()> {
        for name in self
            .enabled
            .iter()
            .chain(&self.disabled)
            .chain(&self.required)
        {
            let _ = name.parse::<SubsystemId>()?;
        }
        for name in &self.enabled {
            let id = name.parse::<SubsystemId>()?;
            if set_contains_subsystem(&self.disabled, id) {
                anyhow::bail!("ACDD subsystem '{name}' cannot be both enabled and disabled");
            }
        }
        if let Some(name) = self.required.iter().find(|name| !self.is_enabled(name)) {
            anyhow::bail!(
                "FEATURE_REQUIRED_BY_PROFILE: ACDD subsystem '{name}' is required but disabled"
            );
        }
        Ok(())
    }

    fn is_raw_subsystem_enabled(&self, id: SubsystemId) -> bool {
        if set_contains_subsystem(&self.disabled, id) {
            return false;
        }
        if set_contains_subsystem(&self.enabled, id) {
            return true;
        }
        if id.is_reserved_default_off() {
            // Reserved slot for AC-006; inactive unless explicitly enabled.
            return false;
        }
        match self.profile {
            SubsystemPreset::Full => true,
            SubsystemPreset::Navigator => matches!(id, SubsystemId::Code | SubsystemId::Docs),
            SubsystemPreset::Orchestrator => {
                matches!(id, SubsystemId::State | SubsystemId::Blackboard)
            }
            SubsystemPreset::Minimal => false,
        }
    }

    /// Return whether a typed built-in subsystem is enabled by the preset and overrides.
    pub fn is_subsystem_enabled(&self, id: SubsystemId) -> bool {
        if id == SubsystemId::Blackboard && !self.is_raw_subsystem_enabled(SubsystemId::State) {
            // Invariant: blackboard physically disables whenever state is disabled.
            return false;
        }
        self.is_raw_subsystem_enabled(id)
    }

    /// Return whether a subsystem (or `domain:<name>`) is enabled by the preset and overrides.
    pub fn is_enabled(&self, name: &str) -> bool {
        if let Some(domain) = name.strip_prefix("domain:") {
            return self.is_domain_enabled(domain);
        }
        name.parse::<SubsystemId>()
            .is_ok_and(|id| self.is_subsystem_enabled(id))
    }

    /// Return whether an external `DomainProvider` plugin is enabled.
    pub fn is_domain_enabled(&self, domain: &str) -> bool {
        self.domains.get(domain).copied().unwrap_or(false)
    }

    /// Return the typed subsystem assigned to an MCP tool, if it is registered here.
    pub fn subsystem_id_for_tool(tool: &str) -> Option<SubsystemId> {
        match tool {
            "acdd_read" | "acdd_guide" => None,
            "acdd_blackboard" => Some(SubsystemId::Blackboard),
            "get_doc" | "search_docs" => Some(SubsystemId::Docs),
            "acdd_state" | "acdd_claim" | "acdd_verify" | "acdd_submit" => Some(SubsystemId::State),
            name if name.starts_with("code_map_") || name == "ast_grep_search" => {
                Some(SubsystemId::Code)
            }
            _ => None,
        }
    }

    /// Return the subsystem assigned to an MCP tool, if it is registered here.
    pub fn subsystem_for_tool(tool: &str) -> Option<&'static str> {
        Self::subsystem_id_for_tool(tool).map(SubsystemId::as_str)
    }
}

/// Keep only tools whose owning subsystem is enabled. Unknown tools remain visible
/// so adding a new tool cannot silently hide it before its owner is assigned.
pub fn filter_subsystem_tools<'a>(
    tools: impl IntoIterator<Item = &'a str>,
    registry: &SubsystemRegistry,
) -> Vec<&'a str> {
    tools
        .into_iter()
        .filter(|tool| {
            SubsystemRegistry::subsystem_id_for_tool(tool)
                .is_none_or(|subsystem| registry.is_subsystem_enabled(subsystem))
        })
        .collect()
}

/// Return a stable error when a request names a tool in a disabled subsystem.
pub fn disabled_tool_error(tool: &str, registry: &SubsystemRegistry) -> Option<String> {
    let subsystem = SubsystemRegistry::subsystem_id_for_tool(tool)?;
    (!registry.is_subsystem_enabled(subsystem)).then(|| {
        format!("FEATURE_DISABLED: MCP tool '{tool}' belongs to disabled subsystem '{subsystem}'")
    })
}

/// Return a disabled-feature error for a layered reader operation.
pub fn disabled_read_mode_error(
    mode: crate::model::ReadMode,
    markdown: bool,
    registry: &SubsystemRegistry,
) -> Option<String> {
    use crate::model::ReadMode;
    let subsystem = match mode {
        ReadMode::Snippet => SubsystemId::Code,
        ReadMode::Outline if markdown => SubsystemId::Docs,
        ReadMode::Outline => SubsystemId::Code,
        _ => return None,
    };
    (!registry.is_subsystem_enabled(subsystem)).then(|| {
        format!(
            "FEATURE_DISABLED: reader mode '{mode:?}' requires disabled subsystem '{subsystem}'"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsystem_id_aliases_parse_and_reject_matrix() {
        let parse_cases: &[(&str, Option<SubsystemId>)] = &[
            ("code", Some(SubsystemId::Code)),
            ("codemap", Some(SubsystemId::Code)),
            ("docs", Some(SubsystemId::Docs)),
            ("state", Some(SubsystemId::State)),
            ("acdd", Some(SubsystemId::State)),
            ("blackboard", Some(SubsystemId::Blackboard)),
            ("memory", Some(SubsystemId::Memory)),
            ("", None),
            ("CODE", None),
            ("unknown", None),
            ("domain:http", None),
        ];
        for &(raw, expected) in parse_cases {
            let parsed = raw.parse::<SubsystemId>().ok();
            assert_eq!(parsed, expected, "parse({raw:?})");
        }

        for id in SubsystemId::ALL {
            assert_eq!(id.to_string(), id.as_str());
            assert_eq!(id.as_str().parse::<SubsystemId>().unwrap(), id);
            for &alias in id.aliases() {
                assert_eq!(alias.parse::<SubsystemId>().unwrap(), id);
            }
            assert_eq!(
                id.is_reserved_default_off(),
                id == SubsystemId::Memory,
                "{id:?}"
            );
        }

        let mut conflict = SubsystemRegistry::default();
        conflict.enabled.insert("code".into());
        conflict.disabled.insert("codemap".into());
        assert!(conflict.validate().is_err());

        let mut unknown = SubsystemRegistry::default();
        unknown.enabled.insert("bogus".into());
        assert!(unknown
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unknown ACDD subsystem 'bogus'"));

        let tool_cases: &[(&str, Option<SubsystemId>, Option<&str>)] = &[
            ("acdd_read", None, None),
            ("acdd_guide", None, None),
            (
                "acdd_blackboard",
                Some(SubsystemId::Blackboard),
                Some("blackboard"),
            ),
            ("get_doc", Some(SubsystemId::Docs), Some("docs")),
            ("search_docs", Some(SubsystemId::Docs), Some("docs")),
            ("acdd_state", Some(SubsystemId::State), Some("state")),
            ("code_map_search", Some(SubsystemId::Code), Some("code")),
            ("ast_grep_search", Some(SubsystemId::Code), Some("code")),
            ("custom_plugin", None, None),
        ];
        for &(tool, expected_id, expected_str) in tool_cases {
            assert_eq!(SubsystemRegistry::subsystem_id_for_tool(tool), expected_id);
            assert_eq!(SubsystemRegistry::subsystem_for_tool(tool), expected_str);
        }
    }

    #[test]
    fn subsystem_registry_aliases_memory_domains_and_blackboard_coupling() {
        let mut reg = SubsystemRegistry::default();
        assert!(reg.is_enabled("code"));
        assert!(reg.is_enabled("codemap"));
        assert!(reg.is_enabled("state"));
        assert!(reg.is_enabled("acdd"));
        assert!(reg.is_enabled("blackboard"));
        assert!(!reg.is_enabled("memory"));
        assert!(!reg.is_domain_enabled("http"));

        reg.disabled.insert("state".into());
        assert!(!reg.is_enabled("state"));
        assert!(!reg.is_enabled("acdd"));
        assert!(!reg.is_enabled("blackboard"));
        assert!(disabled_tool_error("acdd_blackboard", &reg).is_some());
        assert!(disabled_tool_error("acdd_state", &reg).is_some());

        reg.enabled.insert("blackboard".into());
        assert!(
            !reg.is_enabled("blackboard"),
            "blackboard stays disabled while state is disabled"
        );

        reg.disabled.clear();
        reg.enabled.insert("memory".into());
        reg.domains.insert("http".into(), true);
        reg.domains.insert("sql".into(), false);
        assert!(reg.is_enabled("memory"));
        assert!(reg.is_domain_enabled("http"));
        assert!(!reg.is_domain_enabled("sql"));
    }
}
