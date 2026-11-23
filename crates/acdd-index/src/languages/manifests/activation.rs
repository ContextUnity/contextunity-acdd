use crate::models::tags::LanguageId;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Typed activation conditions for framework manifest rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivationCondition {
    /// Activates when the nearest ecosystem manifest declares `name`.
    Package { name: String },
    /// Activates when an indexed file observes exact syntax attribute `name`.
    SyntaxAttribute { name: String },
    /// Activates when an indexed file observes a syntax attribute starting with `prefix`.
    SyntaxAttributePrefix { prefix: String },
    /// Activates when the file extension (without leading dot) equals `extension`.
    FileExtension { extension: String },
    /// Activates when any nested condition matches (bounded nesting depth <= 4).
    Any { conditions: Vec<ActivationCondition> },
}

/// Two-tier evaluation state for workspace-time vs file-syntax-time activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationMatch {
    /// Condition is satisfied at workspace scope.
    Active,
    /// Condition is false at workspace scope and has no file-level conditions.
    Inactive,
    /// Condition depends on per-file extension or syntax activation signals.
    Deferred,
}

/// Manifest-declared syntax attribute names and prefixes for O(1) extractor signal collection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyntaxActivationKeys {
    /// Exact attribute names (for example `x-data`).
    pub exact_attributes: BTreeSet<String>,
    /// Attribute prefixes (for example `x-on:`).
    pub attribute_prefixes: BTreeSet<String>,
}

impl SyntaxActivationKeys {
    /// Reports whether no syntax activation keys are registered.
    pub fn is_empty(&self) -> bool {
        self.exact_attributes.is_empty() && self.attribute_prefixes.is_empty()
    }

    /// Reports whether `attribute` matches any registered exact key or prefix.
    pub fn matches_attribute(&self, attribute: &str) -> bool {
        self.exact_attributes.contains(attribute)
            || self
                .attribute_prefixes
                .iter()
                .any(|prefix| attribute.starts_with(prefix.as_str()))
    }
}

/// Per-file syntax activation signals observed during single-pass extraction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileActivationSignals {
    /// Distinct manifest-declared syntax attributes or matched prefix hits observed in the file.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub syntax_attributes: BTreeSet<String>,
}

impl FileActivationSignals {
    /// Reports whether no syntax activation signals were observed for the file.
    pub fn is_empty(&self) -> bool {
        self.syntax_attributes.is_empty()
    }
}

impl ActivationCondition {
    const MAX_ANY_DEPTH: usize = 4;

    /// Validates one condition, enforcing non-empty values and bounded `Any` nesting.
    pub fn validate(&self) -> Result<()> {
        self.validate_depth(0)
    }

    /// Validates a slice of conditions.
    pub fn validate_slice(conditions: &[Self]) -> Result<()> {
        for condition in conditions {
            condition.validate()?;
        }
        Ok(())
    }

    fn validate_depth(&self, depth: usize) -> Result<()> {
        match self {
            Self::Package { name } => {
                ensure!(
                    !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
                    "activation.name must be nonempty"
                );
            }
            Self::SyntaxAttribute { name } => {
                ensure!(
                    !name.trim().is_empty()
                        && name.len() <= 128
                        && !name.chars().any(|ch| ch.is_whitespace() || ch.is_control()),
                    "activation.syntax_attribute name must be nonempty without whitespace"
                );
            }
            Self::SyntaxAttributePrefix { prefix } => {
                ensure!(
                    !prefix.trim().is_empty()
                        && prefix.len() <= 128
                        && !prefix
                            .chars()
                            .any(|ch| ch.is_whitespace() || ch.is_control()),
                    "activation.syntax_attribute_prefix must be nonempty without whitespace"
                );
            }
            Self::FileExtension { extension } => {
                ensure!(
                    !extension.is_empty()
                        && extension.len() <= 32
                        && !extension.starts_with('.')
                        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()),
                    "activation.file_extension must be nonempty alphanumeric without leading dot"
                );
            }
            Self::Any { conditions } => {
                ensure!(
                    depth < Self::MAX_ANY_DEPTH,
                    "activation.any nesting exceeds maximum depth {}",
                    Self::MAX_ANY_DEPTH
                );
                ensure!(
                    !conditions.is_empty() && conditions.len() <= 32,
                    "activation.any conditions must contain 1..=32 entries"
                );
                for condition in conditions {
                    condition.validate_depth(depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// Reports whether this condition depends on per-file extension or syntax signals.
    pub fn is_file_scoped(&self) -> bool {
        match self {
            Self::Package { .. } => false,
            Self::SyntaxAttribute { .. }
            | Self::SyntaxAttributePrefix { .. }
            | Self::FileExtension { .. } => true,
            Self::Any { conditions } => conditions.iter().any(Self::is_file_scoped),
        }
    }

    /// Collects syntax attribute keys and prefixes referenced by this condition.
    pub fn collect_syntax_keys(&self, keys: &mut SyntaxActivationKeys) {
        match self {
            Self::SyntaxAttribute { name } => {
                keys.exact_attributes.insert(name.clone());
            }
            Self::SyntaxAttributePrefix { prefix } => {
                keys.attribute_prefixes.insert(prefix.clone());
            }
            Self::Any { conditions } => {
                for condition in conditions {
                    condition.collect_syntax_keys(keys);
                }
            }
            Self::Package { .. } | Self::FileExtension { .. } => {}
        }
    }

    /// Evaluates the workspace-level tier (resolving `Package` and deferring file-scoped conditions).
    pub fn matches_workspace(&self, has_package: &dyn Fn(&str) -> bool) -> ActivationMatch {
        match self {
            Self::Package { name } => {
                if has_package(name) {
                    ActivationMatch::Active
                } else {
                    ActivationMatch::Inactive
                }
            }
            Self::SyntaxAttribute { .. }
            | Self::SyntaxAttributePrefix { .. }
            | Self::FileExtension { .. } => ActivationMatch::Deferred,
            Self::Any { conditions } => Self::matches_workspace_slice_dyn(conditions, has_package),
        }
    }

    /// Evaluates a slice of top-level conditions (OR semantics; empty is active).
    pub fn matches_workspace_slice(
        conditions: &[Self],
        has_package: impl Fn(&str) -> bool,
    ) -> ActivationMatch {
        Self::matches_workspace_slice_dyn(conditions, &has_package)
    }

    fn matches_workspace_slice_dyn(
        conditions: &[Self],
        has_package: &dyn Fn(&str) -> bool,
    ) -> ActivationMatch {
        if conditions.is_empty() {
            return ActivationMatch::Active;
        }
        let mut deferred = false;
        for condition in conditions {
            match condition.matches_workspace(has_package) {
                ActivationMatch::Active => return ActivationMatch::Active,
                ActivationMatch::Deferred => deferred = true,
                ActivationMatch::Inactive => {}
            }
        }
        if deferred {
            ActivationMatch::Deferred
        } else {
            ActivationMatch::Inactive
        }
    }

    /// Evaluates file-level extension and syntax signals (when workspace `Package` checks were false).
    pub fn matches_file(&self, path: &str, signals: &FileActivationSignals) -> bool {
        match self {
            Self::Package { .. } => false,
            Self::SyntaxAttribute { name } => signals.syntax_attributes.contains(name.as_str()),
            Self::SyntaxAttributePrefix { prefix } => signals
                .syntax_attributes
                .iter()
                .any(|attribute| attribute.starts_with(prefix.as_str())),
            Self::FileExtension { extension } => std::path::Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value == extension),
            Self::Any { conditions } => Self::matches_file_slice(conditions, path, signals),
        }
    }

    /// Evaluates a slice of conditions at file time (OR semantics; empty is active).
    pub fn matches_file_slice(
        conditions: &[Self],
        path: &str,
        signals: &FileActivationSignals,
    ) -> bool {
        conditions.is_empty()
            || conditions
                .iter()
                .any(|condition| condition.matches_file(path, signals))
    }
}

pub(crate) fn parse_language(value: &str) -> Result<LanguageId> {
    LanguageId::parse(value).with_context(|| format!("unsupported manifest language {value}"))
}
