//! Typed registries shared by ACDD profiles and workspace adapters.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

fn canonical_and_alias(name: &str) -> (&str, Option<&str>) {
    match name {
        "code" | "codemap" => ("code", Some("codemap")),
        "state" | "acdd" => ("state", Some("acdd")),
        other => (other, None),
    }
}

fn set_contains_subsystem(set: &BTreeSet<String>, name: &str) -> bool {
    let (canonical, alias) = canonical_and_alias(name);
    set.contains(canonical) || alias.is_some_and(|a| set.contains(a))
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
            anyhow::ensure!(is_known_subsystem(name), "unknown ACDD subsystem '{name}'");
        }
        for name in &self.enabled {
            if set_contains_subsystem(&self.disabled, name) {
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

    fn is_raw_enabled(&self, name: &str) -> bool {
        if set_contains_subsystem(&self.disabled, name) {
            return false;
        }
        if set_contains_subsystem(&self.enabled, name) {
            return true;
        }
        let (canonical, _) = canonical_and_alias(name);
        if canonical == "memory" {
            // Reserved slot for AC-006; inactive unless explicitly enabled.
            return false;
        }
        match self.profile {
            SubsystemPreset::Full => is_known_subsystem(name),
            SubsystemPreset::Navigator => matches!(canonical, "code" | "docs"),
            SubsystemPreset::Orchestrator => matches!(canonical, "state" | "blackboard"),
            SubsystemPreset::Minimal => false,
        }
    }

    /// Return whether a subsystem (or `domain:<name>`) is enabled by the preset and overrides.
    pub fn is_enabled(&self, name: &str) -> bool {
        if let Some(domain) = name.strip_prefix("domain:") {
            return self.is_domain_enabled(domain);
        }
        let (canonical, _) = canonical_and_alias(name);
        if canonical == "blackboard" && !self.is_raw_enabled("state") {
            // Invariant: blackboard physically disables whenever state is disabled.
            return false;
        }
        self.is_raw_enabled(canonical)
    }

    /// Return whether an external `DomainProvider` plugin is enabled.
    pub fn is_domain_enabled(&self, domain: &str) -> bool {
        self.domains.get(domain).copied().unwrap_or(false)
    }

    /// Return the subsystem assigned to an MCP tool, if it is registered here.
    pub fn subsystem_for_tool(tool: &str) -> Option<&'static str> {
        match tool {
            "acdd_read" | "acdd_guide" => None,
            "acdd_blackboard" => Some("blackboard"),
            "get_doc" | "search_docs" => Some("docs"),
            "acdd_state" | "acdd_claim" | "acdd_verify" | "acdd_submit" => Some("state"),
            name if name.starts_with("code_map_") || name == "ast_grep_search" => Some("code"),
            _ => None,
        }
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
            SubsystemRegistry::subsystem_for_tool(tool)
                .is_none_or(|subsystem| registry.is_enabled(subsystem))
        })
        .collect()
}

/// Return a stable error when a request names a tool in a disabled subsystem.
pub fn disabled_tool_error(tool: &str, registry: &SubsystemRegistry) -> Option<String> {
    let subsystem = SubsystemRegistry::subsystem_for_tool(tool)?;
    (!registry.is_enabled(subsystem)).then(|| {
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
        ReadMode::Snippet => "code",
        ReadMode::Outline if markdown => "docs",
        ReadMode::Outline => "code",
        _ => return None,
    };
    (!registry.is_enabled(subsystem)).then(|| {
        format!(
            "FEATURE_DISABLED: reader mode '{mode:?}' requires disabled subsystem '{subsystem}'"
        )
    })
}

fn is_known_subsystem(name: &str) -> bool {
    matches!(
        name,
        "code" | "codemap" | "state" | "acdd" | "blackboard" | "docs" | "memory"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
