use super::projections::{Api, AttachmentSource, EndpointAttachment, Slot};
use crate::models::tags::LanguageId;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Semantic rule definition loaded from a framework manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRule {
    /// Rule identifier.
    pub id: String,
    /// Semantic namespace.
    pub namespace: String,
    /// Semantic scope.
    pub scope: SemanticScope,
    /// Activation conditions.
    #[serde(default)]
    pub activation: Vec<ActivationCondition>,
    /// Semantic operation performed by this rule.
    pub operation: SemanticOperation,
}

/// Scope of a semantic rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticScope {
    /// Project-wide scope.
    Project,
}

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

/// Callee specification for semantic rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticCallee {
    Imported { module: String, export: String },
}

pub(crate) use crate::semantic::{SemanticRelation, SemanticTarget};

/// Semantic operation variants for framework rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticOperation {
    SymbolAttachment {
        language: String,
        callee: Api,
        source: AttachmentSource,
        target: Slot,
        relation: SemanticRelation,
    },
    EndpointProjection {
        language: String,
        callee: Api,
        attachment: EndpointAttachment,
        url_keyword: String,
        method_keyword: String,
        default_method: String,
    },
    CallToFile {
        language: String,
        callee: SemanticCallee,
        argument: usize,
        #[serde(
            deserialize_with = "file_call_target",
            serialize_with = "serialize_call_target"
        )]
        target: SemanticTarget,
        relation: SemanticRelation,
    },
    CallToSymbol {
        language: String,
        callee: SemanticCallee,
        argument: usize,
        #[serde(
            deserialize_with = "symbol_call_target",
            serialize_with = "serialize_call_target"
        )]
        target: SemanticTarget,
        relation: SemanticRelation,
    },
    ClassAttributeToFile {
        language: String,
        attribute: String,
        #[serde(deserialize_with = "file_attribute_target")]
        target: SemanticTarget,
        relation: SemanticRelation,
    },
    ClassAttributeToSymbol {
        language: String,
        attribute: String,
        #[serde(deserialize_with = "symbol_attribute_target")]
        target: SemanticTarget,
        relation: SemanticRelation,
    },
    ClassAttributeToTable {
        language: String,
        attribute: String,
        #[serde(default)]
        container: Option<String>,
        #[serde(default)]
        bases: Vec<String>,
    },
    ClassDefaultTable {
        language: String,
        bases: Vec<String>,
    },
    MacroTypeToTable {
        language: String,
        macro_path: String,
    },
}

macro_rules! target_decoder {
    ($name:ident, $variant:ident, $message:literal) => {
        fn $name<'de, D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<SemanticTarget, D::Error> {
            let target = SemanticTarget::deserialize(deserializer)?;
            if !matches!(target, SemanticTarget::$variant { .. }) {
                return Err(serde::de::Error::custom($message));
            }
            Ok(target)
        }
    };
}

target_decoder!(
    file_call_target,
    ProjectPathSuffix,
    "call_to_file requires a project_path_suffix target"
);
target_decoder!(
    symbol_call_target,
    ImportedSymbol,
    "call_to_symbol requires an imported_symbol target"
);
target_decoder!(
    file_attribute_target,
    ProjectPathSuffix,
    "class_attribute_to_file requires a project_path_suffix target"
);
target_decoder!(
    symbol_attribute_target,
    PythonSymbol,
    "class_attribute_to_symbol requires a python_symbol target"
);

fn serialize_call_target<S: serde::Serializer>(
    target: &SemanticTarget,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    if matches!(target, SemanticTarget::PythonSymbol { .. }) {
        return Err(serde::ser::Error::custom(
            "call operations cannot target python_symbol",
        ));
    }
    target.serialize(serializer)
}

/// A literal path must already be normal; never repair an uncertain identity.
pub(crate) fn normal_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '\\' | ':' | '?' | '#'))
        })
}

fn identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn validate_prefix(prefix: &str) -> Result<()> {
    ensure!(
        prefix.strip_suffix('/').is_some_and(normal_relative_path),
        "target.prefix needs normal relative directory segments ending in /"
    );
    Ok(())
}

fn validate_class_attribute(
    to_file: bool,
    language: LanguageId,
    attribute: &str,
    target: &SemanticTarget,
    relation: &SemanticRelation,
) -> Result<()> {
    let kind = if to_file {
        "class_attribute_to_file"
    } else {
        "class_attribute_to_symbol"
    };
    ensure!(
        language == LanguageId::Python,
        "unsupported {kind} language {language}"
    );
    ensure!(identifier(attribute), "invalid class attribute");
    if to_file {
        ensure!(
            *relation == SemanticRelation::RendersTemplate,
            "class_attribute_to_file only supports renders_template"
        );
        let SemanticTarget::ProjectPathSuffix { prefix, language } = target else {
            anyhow::bail!("class_attribute_to_file requires project_path_suffix");
        };
        validate_prefix(prefix)?;
        ensure!(
            parse_language(language)? == LanguageId::Html,
            "class_attribute_to_file requires html target language"
        );
    } else {
        ensure!(
            *relation == SemanticRelation::UsesModel,
            "class_attribute_to_symbol only supports uses_model"
        );
        let SemanticTarget::PythonSymbol { language } = target else {
            anyhow::bail!("class_attribute_to_symbol requires python_symbol");
        };
        ensure!(
            parse_language(language)? == LanguageId::Python,
            "class_attribute_to_symbol requires python target language"
        );
    }
    Ok(())
}

impl SemanticRule {
    pub(crate) fn language(&self) -> &str {
        match &self.operation {
            SemanticOperation::SymbolAttachment { language, .. }
            | SemanticOperation::EndpointProjection { language, .. }
            | SemanticOperation::CallToFile { language, .. }
            | SemanticOperation::CallToSymbol { language, .. }
            | SemanticOperation::ClassAttributeToFile { language, .. }
            | SemanticOperation::ClassAttributeToSymbol { language, .. }
            | SemanticOperation::ClassAttributeToTable { language, .. }
            | SemanticOperation::ClassDefaultTable { language, .. }
            | SemanticOperation::MacroTypeToTable { language, .. } => language,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.validate_fields()
            .with_context(|| format!("semantic rule {}", self.id))
    }

    fn validate_fields(&self) -> Result<()> {
        ensure!(!self.id.trim().is_empty(), "id must be nonempty");
        ensure!(
            !self.namespace.trim().is_empty(),
            "namespace must be nonempty"
        );
        ActivationCondition::validate_slice(&self.activation)?;
        match &self.operation {
            SemanticOperation::SymbolAttachment {
                language,
                callee,
                source,
                target,
                relation,
            } => {
                let lang = parse_language(language)?;
                ensure!(
                    lang == LanguageId::Python
                        && matches!(callee, Api::Imported { .. } | Api::ConstructedMember { .. }),
                    "invalid symbol attachment API"
                );
                callee.validate(lang)?;
                target.validate(lang)?;
                ensure!(
                    !matches!(target, Slot::KeywordMember { .. }),
                    "attachment target must be direct"
                );
                ensure!(
                    matches!(
                        relation,
                        SemanticRelation::UsesModel | SemanticRelation::References
                    ),
                    "invalid attachment relation"
                );
                if let AttachmentSource::Argument { index } = source {
                    ensure!(
                        *index < 32
                            && *relation == SemanticRelation::References
                            && !matches!(target, Slot::Argument { index: other } if other == index),
                        "invalid attachment source"
                    );
                }
            }
            SemanticOperation::EndpointProjection {
                language,
                callee,
                attachment,
                url_keyword,
                method_keyword,
                default_method,
            } => {
                let lang = parse_language(language)?;
                ensure!(
                    lang == LanguageId::Python
                        && matches!(callee, Api::Imported { .. })
                        && matches!(attachment.callee, Api::Imported { .. }),
                    "endpoint constructors must be imported Python APIs"
                );
                callee.validate(lang)?;
                attachment.callee.validate(lang)?;
                ensure!(
                    identifier(&attachment.keyword)
                        && identifier(url_keyword)
                        && identifier(method_keyword)
                        && url_keyword != method_keyword
                        && super::projections::method(default_method),
                    "invalid endpoint slots or default method"
                );
            }
            SemanticOperation::CallToFile {
                language,
                callee,
                target,
                relation,
                ..
            }
            | SemanticOperation::CallToSymbol {
                language,
                callee,
                target,
                relation,
                ..
            } => {
                let lang = parse_language(language)?;
                ensure!(
                    lang == LanguageId::Python,
                    "unsupported call operation language {language}"
                );
                let SemanticCallee::Imported { module, export } = callee;
                ensure!(module.split('.').all(identifier), "invalid callee.module");
                ensure!(identifier(export), "invalid callee.export");
                match target {
                    SemanticTarget::ProjectPathSuffix { prefix, language } => {
                        ensure!(
                            *relation == SemanticRelation::RendersTemplate,
                            "call_to_file only supports renders_template"
                        );
                        validate_prefix(prefix)?;
                        ensure!(
                            parse_language(language)? == LanguageId::Html,
                            "unsupported call_to_file target language {language}"
                        );
                    }
                    SemanticTarget::ImportedSymbol { language } => {
                        ensure!(
                            *relation == SemanticRelation::InjectsDependency,
                            "call_to_symbol only supports injects_dependency"
                        );
                        ensure!(
                            parse_language(language)? == LanguageId::Python,
                            "unsupported call_to_symbol target language {language}"
                        );
                    }
                    SemanticTarget::PythonSymbol { .. } => {
                        anyhow::bail!("call operations cannot target python_symbol")
                    }
                }
            }
            SemanticOperation::ClassAttributeToFile {
                language,
                attribute,
                target,
                relation,
            }
            | SemanticOperation::ClassAttributeToSymbol {
                language,
                attribute,
                target,
                relation,
            } => {
                let to_file = matches!(
                    &self.operation,
                    SemanticOperation::ClassAttributeToFile { .. }
                );
                validate_class_attribute(
                    to_file,
                    parse_language(language)?,
                    attribute,
                    target,
                    relation,
                )?;
            }
            SemanticOperation::ClassAttributeToTable {
                language,
                attribute,
                container,
                bases,
            } => {
                ensure!(
                    parse_language(language)? == LanguageId::Python && !attribute.is_empty(),
                    "invalid class attribute table rule"
                );
                ensure!(
                    container.as_ref().is_none_or(|name| !name.is_empty()),
                    "empty container"
                );
                ensure!(
                    bases.iter().all(|base| !base.is_empty()),
                    "empty model base"
                );
            }
            SemanticOperation::ClassDefaultTable { language, bases } => {
                ensure!(
                    parse_language(language)? == LanguageId::Python
                        && !bases.is_empty()
                        && bases.iter().all(|base| !base.is_empty()),
                    "invalid class default table rule"
                );
            }
            SemanticOperation::MacroTypeToTable {
                language,
                macro_path,
            } => {
                ensure!(
                    parse_language(language)? == LanguageId::Rust
                        && !macro_path.is_empty()
                        && !macro_path.chars().any(char::is_whitespace),
                    "invalid macro type table rule"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::languages::manifests::semantic::{normal_relative_path, SemanticRule};

    #[test]
    fn typed_symbol_operation_is_decoded_and_validated() {
        assert!(normal_relative_path("app/providers.py"));
        assert!(!normal_relative_path("app/../providers.py"));

        let rule: SemanticRule = serde_json::from_value(serde_json::json!({
            "id": "fastapi-dependency",
            "namespace": "fastapi",
            "scope": "project",
            "activation": [{"kind": "package", "name": "fastapi"}],
            "operation": {
                "kind": "call_to_symbol",
                "language": "python",
                "callee": {
                    "kind": "imported",
                    "module": "fastapi",
                    "export": "Depends"
                },
                "argument": 0,
                "target": {
                    "kind": "imported_symbol",
                    "language": "python"
                },
                "relation": "injects_dependency"
            }
        }))
        .expect("typed symbol operations must deserialize");

        SemanticRule::validate(&rule).unwrap();
        let encoded = serde_json::to_value(&rule).unwrap();
        assert_eq!(encoded["operation"]["kind"], "call_to_symbol");

        let mut invalid = encoded;
        invalid["operation"]["target"]["language"] = serde_json::json!("ruby");
        let invalid: SemanticRule = serde_json::from_value(invalid).unwrap();
        assert!(SemanticRule::validate(&invalid).is_err());

        let wrong_target = serde_json::json!({
            "id": "wrong-target",
            "namespace": "fastapi",
            "scope": "project",
            "operation": {
                "kind": "call_to_symbol",
                "language": "python",
                "callee": {"kind": "imported", "module": "fastapi", "export": "Depends"},
                "argument": 0,
                "target": {"kind": "project_path_suffix", "prefix": "templates/", "language": "html"},
                "relation": "injects_dependency"
            }
        });
        assert!(serde_json::from_value::<SemanticRule>(wrong_target).is_err());
    }

    #[test]
    fn class_attribute_operations_round_trip_and_reject_wrong_targets() {
        for operation in [
            serde_json::json!({
                "kind": "class_attribute_to_file",
                "language": "python",
                "attribute": "template_name",
                "target": {"kind": "project_path_suffix", "prefix": "templates/", "language": "html"},
                "relation": "renders_template"
            }),
            serde_json::json!({
                "kind": "class_attribute_to_symbol",
                "language": "python",
                "attribute": "model",
                "target": {"kind": "python_symbol", "language": "python"},
                "relation": "uses_model"
            }),
        ] {
            let value = serde_json::json!({
                "id": "django-class-attribute",
                "namespace": "django",
                "scope": "project",
                "activation": [{"kind": "package", "name": "django"}],
                "operation": operation.clone()
            });
            let rule: SemanticRule = serde_json::from_value(value).unwrap();
            SemanticRule::validate(&rule).unwrap();
            let encoded = serde_json::to_value(&rule).unwrap();
            assert_eq!(encoded["operation"], operation);
        }

        let wrong_target = serde_json::json!({
            "id": "django-class-attribute",
            "namespace": "django",
            "scope": "project",
            "operation": {
                "kind": "class_attribute_to_symbol",
                "language": "python",
                "attribute": "model",
                "target": {"kind": "project_path_suffix", "prefix": "templates/", "language": "html"},
                "relation": "uses_model"
            }
        });
        assert!(serde_json::from_value::<SemanticRule>(wrong_target).is_err());

        let wrong_attribute = serde_json::json!({
            "id": "django-class-attribute",
            "namespace": "django",
            "scope": "project",
            "operation": {
                "kind": "class_attribute_to_file",
                "language": "python",
                "attribute": "template",
                "target": {"kind": "project_path_suffix", "prefix": "templates/", "language": "html"},
                "relation": "renders_template"
            }
        });
        let arbitrary_attribute: SemanticRule = serde_json::from_value(wrong_attribute).unwrap();
        SemanticRule::validate(&arbitrary_attribute).unwrap();
    }
}

use crate::semantic::{
    AttachmentRelation, ComponentConfigurationFact, EndpointRequestFact, ExtractionRuleOrigin,
    FactoryResultFact, NamedBindingFact, NamedBindingRole, NamedTargetKind, ObjectMember,
    SourcePosition, StaticString, SymbolAttachmentFact, SymbolOperand, TemplateContextFact,
    ValueFlowFacts,
};
use crate::languages::{semantic::EffectiveSemanticRule, SyntaxContext};

pub(crate) struct SemanticRuleInput<'rules, 'a, 'tree> {
    pub(crate) effective: &'rules EffectiveSemanticRule,
    pub(crate) ctx: &'a SyntaxContext<'a, 'tree>,
    pub(crate) language: &'a str,
    pub(crate) is_call: bool,
    pub(crate) decorated_calls: &'a [tree_sitter::Node<'tree>],
}

type NodeSlotResolver<'a, 'tree> =
    &'a dyn Fn(tree_sitter::Node<'tree>, &Slot) -> Result<Option<tree_sitter::Node<'tree>>, ()>;
type NodeKeywordResolver<'a, 'tree> =
    &'a dyn Fn(tree_sitter::Node<'tree>, &str) -> Result<Option<tree_sitter::Node<'tree>>, ()>;

pub(crate) struct SemanticRuleResolvers<'a, 'rules, 'tree> {
    pub(crate) api: &'a dyn Fn(tree_sitter::Node<'tree>, &'rules Api) -> bool,
    pub(crate) slot: NodeSlotResolver<'a, 'tree>,
    pub(crate) keyword: NodeKeywordResolver<'a, 'tree>,
    pub(crate) literal: &'a dyn Fn(Option<tree_sitter::Node<'tree>>) -> StaticString,
    pub(crate) operand: &'a dyn Fn(Option<tree_sitter::Node<'tree>>) -> SymbolOperand,
    pub(crate) decorated_source: &'a dyn Fn() -> SymbolOperand,
    pub(crate) attached: &'a dyn Fn(tree_sitter::Node<'tree>, &'rules EndpointAttachment) -> bool,
}

pub(crate) fn execute_semantic_rule<'rules, 'tree>(
    input: SemanticRuleInput<'rules, '_, 'tree>,
    resolvers: &SemanticRuleResolvers<'_, 'rules, 'tree>,
    attachments: &mut Vec<SymbolAttachmentFact>,
    endpoints: &mut Vec<EndpointRequestFact>,
) {
    let effective = input.effective;
    let ctx = input.ctx;
    let position = |node: tree_sitter::Node<'_>| SourcePosition {
        line: node.start_position().row + ctx.offset + 1,
        column: node.start_position().column,
    };
    match &effective.rule.operation {
        SemanticOperation::SymbolAttachment {
            language: selected,
            callee,
            source,
            target,
            relation,
        } if selected == input.language => {
            let relation = match relation {
                SemanticRelation::UsesModel => AttachmentRelation::UsesModel,
                SemanticRelation::References => AttachmentRelation::References,
                _ => return,
            };
            let calls = match source {
                AttachmentSource::DecoratedCallable if !input.is_call => input.decorated_calls,
                AttachmentSource::Argument { .. } if input.is_call => std::slice::from_ref(&ctx.node),
                _ => return,
            };
            for &call in calls {
                if !(resolvers.api)(call, callee) {
                    continue;
                }
                let target = match (resolvers.slot)(call, target) {
                    Ok(None) => continue,
                    Ok(Some(node)) => (resolvers.operand)(Some(node)),
                    Err(()) => SymbolOperand::Unknown,
                };
                let source = match source {
                    AttachmentSource::DecoratedCallable => (resolvers.decorated_source)(),
                    AttachmentSource::Argument { index } => (resolvers.operand)(
                        (resolvers.slot)(call, &Slot::Argument { index: *index })
                            .ok()
                            .flatten(),
                    ),
                };
                attachments.push(SymbolAttachmentFact {
                    origin: ExtractionRuleOrigin::new(&effective.pack, &effective.rule.id),
                    position: position(call),
                    source,
                    target,
                    relation,
                });
            }
        }
        SemanticOperation::EndpointProjection {
            language: selected,
            callee,
            attachment,
            url_keyword,
            method_keyword,
            default_method,
        } if selected == input.language && input.is_call => {
            if !(resolvers.api)(ctx.node, callee) || !(resolvers.attached)(ctx.node, attachment) {
                return;
            }
            let url = (resolvers.literal)((resolvers.keyword)(ctx.node, url_keyword).ok().flatten());
            let method = (resolvers.keyword)(ctx.node, method_keyword)
                .map(|node| node.map(|node| (resolvers.literal)(Some(node))));
            let url = match url {
                StaticString::Literal { value }
                    if value.starts_with('/') && !value.starts_with("//") =>
                {
                    StaticString::Literal { value }
                }
                _ => StaticString::Unknown,
            };
            let method = match method {
                Ok(None) => StaticString::Literal {
                    value: default_method.to_owned(),
                },
                Ok(Some(StaticString::Literal { value })) => {
                    let value = value.to_ascii_uppercase();
                    if super::projections::method(&value) {
                        StaticString::Literal { value }
                    } else {
                        StaticString::Unknown
                    }
                }
                _ => StaticString::Unknown,
            };
            endpoints.push(EndpointRequestFact {
                origin: ExtractionRuleOrigin::new(&effective.pack, &effective.rule.id),
                position: position(ctx.node),
                url,
                method,
            });
        }
        _ => {}
    }
}

pub(crate) enum ProjectionPayload {
    Factory {
        binding: String,
        members: Vec<ObjectMember>,
    },
    Named {
        key: StaticString,
        target: Option<(SourcePosition, Option<String>)>,
    },
}

pub(crate) fn syntax_rule_api(rule: &super::syntax::SyntaxRule) -> Option<Result<&Api, &str>> {
    use super::{projections::NamedBindingForm, syntax::SyntaxOperation};
    match &rule.operation {
        SyntaxOperation::FactoryMembers { callee, .. }
        | SyntaxOperation::NamedBinding {
            binding:
                NamedBindingForm::CallDefinition { callee, .. }
                | NamedBindingForm::CallReference { callee, .. },
            ..
        } => Some(Ok(callee)),
        SyntaxOperation::NamedBinding {
            binding: NamedBindingForm::MemberReference { root },
            ..
        } => Some(Err(root)),
        _ => None,
    }
}

pub(crate) fn bind_receiver_type(
    rule: &super::syntax::SyntaxRule,
    position: SourcePosition,
    payload: ProjectionPayload,
    facts: &crate::models::Facts,
    flow: &mut ValueFlowFacts,
) {
    use super::syntax::SyntaxOperation;
    match (&rule.operation, payload) {
        (
            SyntaxOperation::FactoryMembers {
                configured_module, ..
            },
            ProjectionPayload::Factory { binding, members },
        ) => flow.push_factory_result(FactoryResultFact {
            origin: ExtractionRuleOrigin::new(&rule.pack, &rule.id),
            position,
            binding,
            configured_module: configured_module.clone(),
            members,
        }),
        (
            SyntaxOperation::NamedBinding { domain, binding },
            ProjectionPayload::Named { key, target },
        ) => flow.push_named_binding(emit_named_binding(
            NamedBindingRequest {
                rule,
                position,
                domain,
                binding,
                key,
                target,
            },
            facts,
        )),
        _ => {}
    }
}

struct NamedBindingRequest<'a> {
    rule: &'a super::syntax::SyntaxRule,
    position: SourcePosition,
    domain: &'a str,
    binding: &'a super::projections::NamedBindingForm,
    key: StaticString,
    target: Option<(SourcePosition, Option<String>)>,
}

fn emit_named_binding(
    req: NamedBindingRequest<'_>,
    facts: &crate::models::Facts,
) -> NamedBindingFact {
    let role =
        if let super::projections::NamedBindingForm::CallDefinition { target_kind, .. } = req.binding
        {
            let kind = match target_kind {
                super::projections::DefinitionKind::Object => NamedTargetKind::Object,
                super::projections::DefinitionKind::Callable => NamedTargetKind::Callable,
            };
            let operand = req.target.map_or(SymbolOperand::Unknown, |(position, name)| {
                if let Some(expression) = name {
                    return SymbolOperand::Name { expression };
                }
                let mut nodes = facts.nodes.iter().filter(|node| {
                    node.line == position.line
                        && node.details["column"].as_u64().unwrap_or(0) == position.column as u64
                        && match kind {
                            NamedTargetKind::Object => node.kind == "object",
                            NamedTargetKind::Callable => {
                                matches!(node.kind.as_str(), "function" | "method")
                            }
                            NamedTargetKind::ListenerScope => false,
                        }
                });
                let Some(node) = nodes.next() else {
                    return SymbolOperand::Unknown;
                };
                if nodes.next().is_some() {
                    SymbolOperand::Unknown
                } else {
                    SymbolOperand::Node {
                        id: node.id.clone(),
                    }
                }
            });
            NamedBindingRole::Definition {
                target: operand,
                target_kind: kind,
            }
        } else {
            NamedBindingRole::Reference
        };
    NamedBindingFact {
        origin: ExtractionRuleOrigin::new(&req.rule.pack, &req.rule.id),
        position: req.position,
        domain: req.domain.to_owned(),
        key: req.key,
        role,
    }
}

pub(crate) struct SyntaxRuleInput<'a> {
    pub(crate) rule: &'a super::syntax::SyntaxRule,
    pub(crate) is_call: bool,
    pub(crate) callee: &'a str,
    pub(crate) offset: usize,
}

type FactoryMembersResolver<'a> =
    &'a dyn Fn(usize, &str, &str) -> Option<(String, Vec<ObjectMember>, Vec<ObjectMember>)>;
type SlotTargetResolver<'a> = &'a dyn Fn(&Slot) -> Option<(SourcePosition, Option<String>)>;

pub(crate) struct SyntaxRuleResolvers<'a> {
    pub(crate) api_shape: &'a dyn Fn(&Api, &str) -> bool,
    pub(crate) outermost_member: &'a dyn Fn() -> bool,
    pub(crate) factory: FactoryMembersResolver<'a>,
    pub(crate) setup: &'a dyn Fn(usize) -> Option<(String, Vec<ObjectMember>)>,
    pub(crate) argument: SlotTargetResolver<'a>,
    pub(crate) literal: &'a dyn Fn(&Slot) -> Option<StaticString>,
}

pub(crate) fn prepare_syntax_rule(
    input: SyntaxRuleInput<'_>,
    resolvers: &SyntaxRuleResolvers<'_>,
) -> Option<ProjectionPayload> {
    use super::{projections::NamedBindingForm, syntax::SyntaxOperation};
    match &input.rule.operation {
        SyntaxOperation::FactoryMembers {
            callee: api,
            argument,
            state_member,
            actions_member,
            shape,
            ..
        } if input.is_call && (resolvers.api_shape)(api, input.callee) => {
            let (binding, mut members) = if *shape == super::syntax::FactoryShape::SetupReturn {
                (resolvers.setup)(*argument)?
            } else {
                let (binding, mut members, methods) =
                    (resolvers.factory)(*argument, state_member, actions_member)?;
                if members.len() + methods.len() > 128
                    || methods
                        .iter()
                        .any(|method| members.iter().any(|field| field.name == method.name))
                {
                    return None;
                }
                members.extend(methods);
                (binding, members)
            };
            for member in &mut members {
                member.position.line += input.offset;
            }
            Some(ProjectionPayload::Factory { binding, members })
        }
        SyntaxOperation::NamedBinding {
            binding:
                NamedBindingForm::CallDefinition {
                    callee: api,
                    key,
                    target,
                    ..
                },
            ..
        } if input.is_call && (resolvers.api_shape)(api, input.callee) => {
            Some(ProjectionPayload::Named {
                key: (resolvers.literal)(key)?,
                target: (resolvers.argument)(target),
            })
        }
        SyntaxOperation::NamedBinding {
            binding:
                NamedBindingForm::CallReference {
                    callee: api, key, ..
                },
            ..
        } if input.is_call && (resolvers.api_shape)(api, input.callee) => {
            Some(ProjectionPayload::Named {
                key: (resolvers.literal)(key)?,
                target: None,
            })
        }
        SyntaxOperation::NamedBinding {
            binding: NamedBindingForm::MemberReference { root },
            ..
        } if input.callee.split('.').count() >= 2
            && input.callee.split('.').next() == Some(root)
            && (resolvers.outermost_member)() =>
        {
            Some(ProjectionPayload::Named {
                key: StaticString::Literal {
                    value: input
                        .callee
                        .split('.')
                        .nth(1)
                        .expect("member path")
                        .to_owned(),
                },
                target: None,
            })
        }
        _ => None,
    }
}

pub(crate) fn execute_reference_rule<'rules>(
    rule: &'rules super::syntax::SyntaxRule,
    position: SourcePosition,
    facts: &crate::models::Facts,
    api: impl Fn(&'rules Api) -> bool,
    key: impl Fn(&Slot) -> Result<Option<StaticString>, ()>,
) -> Option<NamedBindingFact> {
    let super::syntax::SyntaxOperation::NamedBinding {
        domain,
        binding:
            binding @ super::projections::NamedBindingForm::CallReference {
                callee,
                key: selected,
                ..
            },
    } = &rule.operation
    else {
        return None;
    };
    if !api(callee) {
        return None;
    }
    let key = match key(selected) {
        Ok(None) => return None,
        Ok(Some(value)) => value,
        Err(()) => StaticString::Unknown,
    };
    Some(emit_named_binding(
        NamedBindingRequest {
            rule,
            position,
            domain,
            binding,
            key,
            target: None,
        },
        facts,
    ))
}

pub(crate) struct ContextKeyRequest<'a> {
    pub(crate) rules: &'a [super::syntax::SyntaxRule],
    pub(crate) candidates: &'a [usize],
    pub(crate) argument_count: usize,
    pub(crate) alias: &'a str,
    pub(crate) position: SourcePosition,
}

pub(crate) struct ContextKeyResolvers<'a> {
    pub(crate) context_shape: &'a dyn Fn(usize) -> bool,
    pub(crate) literal: &'a dyn Fn(usize) -> Option<String>,
    pub(crate) imported: &'a dyn Fn(&str, &str) -> bool,
    pub(crate) context_keys: &'a dyn Fn(usize) -> Option<Vec<String>>,
}

pub(crate) fn project_context_key(
    req: ContextKeyRequest<'_>,
    resolvers: &ContextKeyResolvers<'_>,
) -> Option<TemplateContextFact> {
    use super::syntax::SyntaxOperation;
    let (rule, target_index, context_index) = req.candidates.iter().find_map(|&index| {
        let rule = &req.rules[index];
        let SyntaxOperation::RenderContext {
            module,
            export,
            target_argument,
            context_argument,
            argument_count: expected,
            ..
        } = &rule.operation
        else {
            return None;
        };
        (req.argument_count == *expected && (resolvers.imported)(module, export)).then_some((
            rule,
            *target_argument,
            *context_argument,
        ))
    })?;
    let target = (resolvers.literal)(target_index)?;
    if target.is_empty() || target.len() > 256 || !(resolvers.context_shape)(context_index) {
        return None;
    }
    let keys = (resolvers.context_keys)(context_index)?;
    if keys.is_empty() || keys.len() > 32 {
        return None;
    }
    let mut unique = std::collections::HashSet::new();
    if keys.iter().any(|key| {
        key.is_empty()
            || key.len() > 128
            || !key.chars().enumerate().all(|(index, ch)| {
                ch == '_'
                    || if index == 0 {
                        ch.is_alphabetic()
                    } else {
                        ch.is_alphanumeric()
                    }
            })
            || !unique.insert(key)
    }) {
        return None;
    }
    Some(TemplateContextFact {
        origin: Some(ExtractionRuleOrigin::new(&rule.pack, &rule.id)),
        import_alias: req.alias.to_owned(),
        target,
        target_path: None,
        keys,
        position: req.position,
    })
}

pub(crate) struct ClassAttributeRequest<'a> {
    pub(crate) effective: &'a EffectiveSemanticRule,
    pub(crate) language: &'a str,
    pub(crate) attribute: &'a str,
    pub(crate) owner: &'a str,
    pub(crate) position: SourcePosition,
    pub(crate) expression: &'a str,
}

pub(crate) struct ClassAttributeResolvers<'a> {
    pub(crate) literal: &'a dyn Fn() -> Option<String>,
    pub(crate) symbol: &'a dyn Fn() -> Option<String>,
}

pub(crate) fn execute_class_attribute_rule(
    req: ClassAttributeRequest<'_>,
    resolvers: &ClassAttributeResolvers<'_>,
    facts: &mut crate::models::Facts,
) {
    use crate::semantic::SemanticFactRole;
    if let SemanticOperation::ClassAttributeToTable {
        language: selected,
        attribute: expected,
        container,
        ..
    } = &req.effective.rule.operation
    {
        if selected != req.language || expected != req.attribute {
            return;
        }
        let Some(value) = (resolvers.literal)().filter(|value| !value.is_empty()) else {
            return;
        };
        let target_owner = if let Some(container) = container {
            if !facts
                .nodes
                .iter()
                .any(|node| node.id == req.owner && node.kind == "class" && node.name == *container)
            {
                return;
            }
            let Some(parent) = facts
                .edges
                .iter()
                .find(|edge| edge.kind == "contains" && edge.dst == req.owner)
                .map(|edge| edge.src.clone())
            else {
                return;
            };
            parent
        } else {
            req.owner.to_owned()
        };
        facts.references.push(crate::models::Reference {
            source: target_owner,
            expression: value,
            kind: "defines_table".into(),
            line: req.position.line,
            column: req.position.column,
            alias: None,
            module: None,
            dynamic: false,
            receiver_hint: None,
        });
        return;
    }
    let (identity, role, target) = match &req.effective.rule.operation {
        SemanticOperation::ClassAttributeToFile {
            language: selected,
            attribute: expected,
            target,
            relation,
        } if selected == req.language && expected == req.attribute => (
            (resolvers.literal)()
                .filter(|value| normal_relative_path(value))
                .unwrap_or_default(),
            SemanticFactRole::ClassAttributeToFile {
                attribute: req.attribute.to_owned(),
                relation: *relation,
            },
            target,
        ),
        SemanticOperation::ClassAttributeToSymbol {
            language: selected,
            attribute: expected,
            target,
            relation,
        } if selected == req.language && expected == req.attribute => (
            (resolvers.symbol)().unwrap_or_default(),
            SemanticFactRole::ClassAttributeToSymbol {
                attribute: req.attribute.to_owned(),
                relation: *relation,
            },
            target,
        ),
        _ => return,
    };
    crate::languages::semantic::push(
        facts,
        req.effective.fact(
            req.owner,
            req.position.line,
            req.position.column,
            identity,
            req.expression.to_owned(),
            target,
            role,
        ),
    );
}

pub(crate) fn execute_registry_rule(
    rule: &super::syntax::SyntaxRule,
    position: SourcePosition,
    member: Option<&str>,
    key: StaticString,
    receiver: impl FnOnce() -> SymbolOperand,
    target: impl FnOnce() -> SymbolOperand,
) -> Option<NamedBindingFact> {
    let super::syntax::SyntaxOperation::NamedBinding {
        domain,
        binding:
            super::projections::NamedBindingForm::AssignmentDefinition {
                member: expected, ..
            },
    } = &rule.operation
    else {
        return None;
    };
    if member != Some(expected) {
        return None;
    }
    Some(NamedBindingFact {
        origin: ExtractionRuleOrigin::new(&rule.pack, &rule.id),
        position,
        domain: domain.clone(),
        key,
        role: NamedBindingRole::RegistryDefinition {
            receiver: receiver(),
            target: target(),
        },
    })
}

pub(crate) fn execute_named_route_rule(
    rule: &super::syntax::SyntaxRule,
    owner: &str,
    position: SourcePosition,
    prove: impl FnOnce(&str, &str, usize) -> Option<String>,
    facts: &mut crate::models::Facts,
) {
    let super::syntax::SyntaxOperation::NamedRouteCall {
        module,
        export,
        argument,
        expression_prefix,
    } = &rule.operation
    else {
        return;
    };
    let Some(name) = prove(module, export, *argument).filter(|name| {
        !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
    }) else {
        return;
    };
    facts.references.push(crate::models::Reference {
        source: owner.to_owned(),
        dynamic: false,
        expression: format!("{expression_prefix}{name}"),
        kind: "references_route".into(),
        line: position.line,
        column: position.column,
        alias: None,
        module: None,
        receiver_hint: Some(crate::models::ReceiverHint::NamedRoute {
            origin: ExtractionRuleOrigin::new(&rule.pack, &rule.id),
            key: name,
        }),
    });
}

pub(crate) fn execute_configuration_rule(
    rule: &super::syntax::SyntaxRule,
    path: &str,
    enabled: impl FnOnce(&str, &str) -> Option<bool>,
    declared: impl Fn(&str, &str, &str) -> bool,
    callable: impl FnOnce(
        &str,
        &super::syntax::CallableImports,
    ) -> Option<crate::semantic::CallableImportConfiguration>,
) -> Option<crate::semantic::ComponentConfigurationFact> {
    let super::syntax::SyntaxOperation::ComponentEnvironment {
        config_file,
        config_callee,
        enabled_key,
        modules_key,
        modules,
        callable_imports,
        ..
    } = &rule.operation
    else {
        return None;
    };
    if path.rsplit('/').next() != Some(config_file.as_str()) {
        return None;
    }
    Some(crate::semantic::ComponentConfigurationFact {
        origin: ExtractionRuleOrigin::new(&rule.pack, &rule.id),
        enabled: enabled(config_callee, enabled_key)?,
        modules: modules
            .keys()
            .filter(|name| declared(config_callee, modules_key, name))
            .cloned()
            .collect(),
        callable_imports: callable_imports
            .as_ref()
            .and_then(|descriptor| callable(config_callee, descriptor)),
    })
}

pub(crate) fn object_factory_matches(rule: &super::syntax::SyntaxRule, callee: &str) -> bool {
    matches!(&rule.operation, super::syntax::SyntaxOperation::ObjectFactory { callees, .. }
        if callees.iter().any(|configured| callee == configured || (configured.starts_with('.') && callee.ends_with(configured))))
}

pub(crate) fn object_factory_guard(rule: &super::syntax::SyntaxRule, binding: &str) -> bool {
    matches!(&rule.operation, super::syntax::SyntaxOperation::ObjectFactory { unshadowed_globals, .. }
        if unshadowed_globals.iter().any(|name| name == binding))
}

pub(crate) fn execute_object_factory_rule(
    rule: &super::syntax::SyntaxRule,
    callee: &str,
) -> Option<crate::semantic::ObjectOwnershipFact> {
    object_factory_matches(rule, callee).then(|| {
        let binding = callee.split('.').next().unwrap_or("");
        crate::semantic::ObjectOwnershipFact {
            origin: ExtractionRuleOrigin::new(&rule.pack, &rule.id),
            guard: object_factory_guard(rule, binding).then(|| binding.to_owned()),
        }
    })
}

pub(crate) fn projection_object_matches(
    rule: &super::syntax::SyntaxRule,
    api: impl Fn(&Api) -> bool,
    target: impl FnOnce(&Slot) -> bool,
    members: impl FnOnce(usize, &str, &str) -> bool,
) -> bool {
    use super::{
        projections::{DefinitionKind, NamedBindingForm},
        syntax::SyntaxOperation,
    };
    match &rule.operation {
        SyntaxOperation::NamedBinding {
            binding:
                NamedBindingForm::CallDefinition {
                    callee,
                    target: slot,
                    target_kind: DefinitionKind::Object,
                    ..
                },
            ..
        } => api(callee) && target(slot),
        SyntaxOperation::FactoryMembers {
            callee,
            argument,
            state_member,
            actions_member,
            ..
        } => api(callee) && members(*argument, state_member, actions_member),
        _ => false,
    }
}

pub(crate) struct CallRulesInput<'a, 'source, 'tree> {
    pub(crate) ctx: &'a crate::languages::SyntaxContext<'source, 'tree>,
    pub(crate) rules: &'a crate::languages::LanguageContext,
    pub(crate) indices: &'a [usize],
    pub(crate) language: &'a str,
    pub(crate) alias: &'a str,
}

pub(crate) struct CallRulesResolvers<'a, 'source> {
    pub(crate) candidate: &'a dyn Fn(&str) -> bool,
    pub(crate) argument_value: &'a dyn Fn(usize) -> Option<&'source str>,
    pub(crate) imported: &'a dyn Fn(&str, &str) -> bool,
}

pub(crate) fn execute_call_rules<'source>(
    input: CallRulesInput<'_, 'source, '_>,
    resolvers: &CallRulesResolvers<'_, 'source>,
    facts: &mut crate::models::Facts,
) {
    let ctx = input.ctx;
    let Some(module_owner) = facts
        .nodes
        .iter()
        .find(|node| node.kind == "module")
        .map(|node| node.id.clone())
    else {
        return;
    };
    let position = ctx.node.start_position();
    let call_line = position.row + ctx.offset + 1;
    let mut visible_local_import_owners = None;
    for &index in input.indices {
        let effective = &input.rules.semantic_rules[index];
        let (SemanticOperation::CallToFile {
            language: expected,
            callee,
            argument,
            target,
            relation,
        }
        | SemanticOperation::CallToSymbol {
            language: expected,
            callee,
            argument,
            target,
            relation,
        }) = &effective.rule.operation
        else {
            continue;
        };
        let SemanticCallee::Imported { module, export } = callee;
        if expected != input.language || !(resolvers.candidate)(module) {
            continue;
        }
        let Some(literal) =
            (resolvers.argument_value)(*argument).filter(|literal| normal_relative_path(literal))
        else {
            continue;
        };
        let Some(import_owner) = facts
            .references
            .iter()
            .find(|reference| {
                reference.kind == "imports"
                    && reference.alias.as_deref().unwrap_or(&reference.expression) == input.alias
                    && reference.module.as_deref() == Some(module)
                    && reference.expression == *export
                    && import_scope_visible_from_call(
                        &reference.source,
                        &module_owner,
                        ctx.owner,
                        call_line,
                        &mut visible_local_import_owners,
                        facts,
                    )
            })
            .map(|reference| reference.source.clone())
        else {
            continue;
        };
        if !(resolvers.imported)(module, export) {
            continue;
        }
        crate::languages::semantic::push(
            facts,
            effective.fact(
                ctx.owner,
                position.row + ctx.offset + 1,
                position.column,
                literal.to_owned(),
                crate::languages::text(ctx.node, ctx.source).to_owned(),
                target,
                crate::languages::semantic::SemanticFactRole::CallToFile {
                    import_owner,
                    alias: input.alias.to_owned(),
                    module: module.clone(),
                    export: export.clone(),
                    relation: *relation,
                },
            ),
        );
    }
}

pub(crate) fn semantic_rule_apis(rule: &SemanticRule) -> [Option<&Api>; 2] {
    match &rule.operation {
        SemanticOperation::SymbolAttachment { callee, .. } => [Some(callee), None],
        SemanticOperation::EndpointProjection {
            callee, attachment, ..
        } => [Some(callee), Some(&attachment.callee)],
        _ => [None, None],
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedRules {
    pub(crate) render_rules: Vec<usize>,
    pub(crate) loader_rules: Vec<usize>,
    pub(crate) named_route_rules: Vec<usize>,
    pub(crate) named_route_members: hashbrown::HashMap<String, Vec<usize>>,
    pub(crate) decorated_registration_rules: Vec<usize>,
    pub(crate) class_attributes: hashbrown::HashMap<String, Vec<usize>>,
    pub(crate) class_defaults: Vec<usize>,
    pub(crate) macro_tables: hashbrown::HashMap<String, Vec<usize>>,
    pub(crate) function_semantic: Vec<usize>,
    pub(crate) call_semantic: Vec<usize>,
    pub(crate) call_to_file: Vec<usize>,
    pub(crate) named_references: Vec<usize>,
    pub(crate) registry_definitions: Vec<usize>,
    pub(crate) semantic_constructors: Vec<usize>,
    pub(crate) syntax_constructors: Vec<usize>,
}

pub(crate) fn prepare_projection_rules(
    rules: &crate::languages::LanguageContext,
) -> std::collections::HashMap<String, PreparedRules> {
    use super::{projections::NamedBindingForm, syntax::SyntaxOperation};
    let mut compiled = std::collections::HashMap::<String, PreparedRules>::new();
    for (index, effective) in rules
        .semantic_rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| rule.active)
    {
        match &effective.rule.operation {
            SemanticOperation::ClassAttributeToFile {
                language,
                attribute,
                ..
            }
            | SemanticOperation::ClassAttributeToSymbol {
                language,
                attribute,
                ..
            }
            | SemanticOperation::ClassAttributeToTable {
                language,
                attribute,
                ..
            } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .class_attributes
                    .entry(attribute.clone())
                    .or_default()
                    .push(index);
            }
            SemanticOperation::CallToFile { language, .. }
            | SemanticOperation::CallToSymbol { language, .. } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .call_to_file
                    .push(index);
            }
            SemanticOperation::ClassDefaultTable { language, .. } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .class_defaults
                    .push(index);
            }
            SemanticOperation::MacroTypeToTable {
                language,
                macro_path,
            } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .macro_tables
                    .entry(macro_path.clone())
                    .or_default()
                    .push(index);
            }
            SemanticOperation::SymbolAttachment {
                language,
                callee,
                source,
                ..
            } => {
                let prepared = compiled.entry(language.clone()).or_default();
                match source {
                    AttachmentSource::DecoratedCallable => prepared.function_semantic.push(index),
                    AttachmentSource::Argument { .. } => prepared.call_semantic.push(index),
                }
                if matches!(callee, Api::ConstructedMember { .. }) {
                    prepared.semantic_constructors.push(index);
                }
            }
            SemanticOperation::EndpointProjection {
                language,
                callee,
                attachment,
                ..
            } => {
                let prepared = compiled.entry(language.clone()).or_default();
                prepared.call_semantic.push(index);
                if [callee, &attachment.callee]
                    .iter()
                    .any(|api| matches!(api, Api::ConstructedMember { .. }))
                {
                    prepared.semantic_constructors.push(index);
                }
            }
        }
    }
    for (index, rule) in rules.syntax_rules.iter().enumerate() {
        let prepared = compiled.entry(rule.language.clone()).or_default();
        match &rule.operation {
            SyntaxOperation::RenderContext { .. } => prepared.render_rules.push(index),
            SyntaxOperation::TemplateLoader { .. } => prepared.loader_rules.push(index),
            SyntaxOperation::NamedRouteCall { export, .. } => {
                prepared.named_route_rules.push(index);
                prepared
                    .named_route_members
                    .entry(export.clone())
                    .or_default()
                    .push(index);
            }
            SyntaxOperation::DecoratedRegistration { .. } => {
                prepared.decorated_registration_rules.push(index)
            }
            SyntaxOperation::NamedBinding {
                binding: NamedBindingForm::CallReference { callee, .. },
                ..
            } => {
                prepared.named_references.push(index);
                if matches!(callee, Api::ConstructedMember { .. }) {
                    prepared.syntax_constructors.push(index);
                }
            }
            SyntaxOperation::NamedBinding {
                binding: NamedBindingForm::AssignmentDefinition { .. },
                ..
            } => {
                prepared.registry_definitions.push(index);
            }
            _ => {}
        }
    }
    compiled
}

pub(crate) fn imported_rule_candidates(
    rules: &[super::syntax::SyntaxRule],
    indices: &[usize],
    imports: &std::collections::HashMap<(&str, &str), Vec<(&str, usize)>>,
) -> std::collections::HashMap<String, Vec<usize>> {
    use super::syntax::SyntaxOperation;
    let mut candidates = std::collections::HashMap::<String, Vec<usize>>::new();
    for (index, rule) in indexed_rules(rules, indices.iter().copied()) {
        let (module, export) = match &rule.operation {
            SyntaxOperation::RenderContext { module, export, .. }
            | SyntaxOperation::NamedRouteCall { module, export, .. } => (module, export),
            SyntaxOperation::TemplateLoader {
                module,
                environment,
                ..
            } => (module, environment),
            _ => continue,
        };
        for &(alias, _) in imports
            .get(&(module.as_str(), export.as_str()))
            .into_iter()
            .flatten()
        {
            candidates.entry(alias.to_owned()).or_default().push(index);
        }
    }
    candidates
}

pub(crate) fn indexed_rules<T, I>(rules: &[T], indices: I) -> impl Iterator<Item = (usize, &T)>
where
    I: IntoIterator<Item = usize>,
{
    indices.into_iter().map(move |index| (index, &rules[index]))
}

use crate::models::{Facts, Node, ReceiverHint};
use crate::semantic::ValueExpr;
use crate::typed_facts::FlowStore;

fn has_name(node: &Node, field: &str, name: &str) -> bool {
    match field {
        "bindings" => node.binding_names().any(|binding| binding == name),
        "rebindings" => node.rebinding_names().any(|binding| binding == name),
        _ => node.details[field]
            .as_array()
            .is_some_and(|names| names.iter().any(|value| value.as_str() == Some(name))),
    }
}

pub(crate) fn lexical_scopes<'a>(owner: &'a Node, facts: &'a Facts) -> Vec<&'a Node> {
    let mut scopes: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| {
            node.id == owner.id
                || node.kind == "module"
                || (owner
                    .qualname
                    .strip_prefix(&node.qualname)
                    .is_some_and(|suffix| suffix.starts_with('.'))
                    && node.has_binding_names())
        })
        .collect();
    scopes.sort_unstable_by_key(|node| std::cmp::Reverse(node.qualname.len()));
    scopes
}

fn import_scope_visible_from_call(
    source: &str,
    module_owner: &str,
    call_owner: &str,
    call_line: usize,
    visible_local_import_owners: &mut Option<Vec<String>>,
    facts: &Facts,
) -> bool {
    if source == module_owner || source == call_owner {
        return true;
    }
    if source.starts_with("module:") {
        return false;
    }
    visible_local_import_owners
        .get_or_insert_with(|| visible_local_import_owners_for_call(call_owner, call_line, facts))
        .iter()
        .any(|owner| owner.as_str() == source)
}

fn visible_local_import_owners_for_call(
    call_owner: &str,
    call_line: usize,
    facts: &Facts,
) -> Vec<String> {
    let Some(call_scope) = facts.nodes.iter().find(|node| node.id == call_owner) else {
        return Vec::new();
    };
    let call_qualname = call_scope.qualname.as_str();
    facts
        .nodes
        .iter()
        .filter(|scope| {
            scope.id != call_owner
                && matches!(scope.kind.as_str(), "function" | "method")
                && scope.line <= call_line
                && call_line <= scope.end_line
                && scope.has_binding_names()
                && call_qualname
                    .strip_prefix(&scope.qualname)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
        .map(|scope| scope.id.clone())
        .collect()
}

fn import_binds(reference: &crate::models::Reference, scope: &Node, name: &str) -> bool {
    reference.source == scope.id
        && reference.kind == "imports"
        && reference.alias.as_deref().unwrap_or(&reference.expression) == name
}

pub(crate) fn unshadowed(scopes: &[&Node], name: &str, facts: &Facts) -> bool {
    !scopes.iter().any(|scope| {
        has_name(scope, "bindings", name)
            || has_name(scope, "rebindings", name)
            || facts
                .references
                .iter()
                .any(|reference| import_binds(reference, scope, name))
    })
}

pub(crate) fn unique_initializer<'a>(
    flow: &'a ValueFlowFacts,
    name: &str,
) -> Option<(
    &'a crate::semantic::BindingFact,
    &'a crate::semantic::BindingFact,
)> {
    let writes: Vec<_> = flow
        .bindings
        .iter()
        .filter(|write| write.name == name)
        .collect();
    let [first, second] = writes.as_slice() else {
        return None;
    };
    if first.conditional || second.conditional {
        return None;
    }
    let (declaration, initializer) = match (&first.value, &second.value) {
        (ValueExpr::Unknown, value) if !matches!(value, ValueExpr::Unknown) => (*first, *second),
        (value, ValueExpr::Unknown) if !matches!(value, ValueExpr::Unknown) => (*second, *first),
        _ => return None,
    };
    (declaration.position < initializer.position).then_some((declaration, initializer))
}

pub(crate) fn api_proven(
    api: &Api,
    callee: &str,
    owner: &Node,
    position: SourcePosition,
    facts: &Facts,
    flows: &FlowStore,
    file_import_mask: u64,
) -> bool {
    let scopes = lexical_scopes(owner, facts);
    match api {
        Api::Global { name } => callee == name && unshadowed(&scopes, name, facts),
        Api::GlobalMember { receiver, member } => {
            let Some(actual) = callee
                .strip_suffix(member.as_str())
                .and_then(|head| head.strip_suffix('.'))
            else {
                return false;
            };
            let root = receiver.split('.').next().expect("validated receiver");
            if !unshadowed(&scopes, root, facts) {
                return false;
            }
            if actual == receiver {
                return true;
            }
            if actual.contains('.') {
                return false;
            }
            let Some(scope) = scopes
                .iter()
                .find(|scope| has_name(scope, "bindings", actual))
            else {
                return false;
            };
            let Some(flow) = flows.get(scope) else {
                return false;
            };
            let Some((_, binding)) = unique_initializer(flow, actual) else {
                return false;
            };
            if binding.position >= position {
                return false;
            }
            match &binding.value {
                ValueExpr::Alias { name } => name == receiver,
                ValueExpr::Field {
                    receiver: base,
                    member,
                } => {
                    receiver
                        .strip_prefix(base.as_str())
                        .and_then(|tail| tail.strip_prefix('.'))
                        == Some(member.as_str())
                }
                _ => false,
            }
        }
        Api::Imported { module, export } => {
            let required_mask = crate::interner::package_mask(module);
            if file_import_mask & required_mask != required_mask {
                return false;
            }
            let (alias, member) = callee
                .split_once('.')
                .map_or((callee, None), |(alias, member)| (alias, Some(member)));
            let Some(scope) = scopes.iter().find(|scope| {
                has_name(scope, "bindings", alias)
                    || facts
                        .references
                        .iter()
                        .any(|reference| import_binds(reference, scope, alias))
            }) else {
                return false;
            };
            if has_name(scope, "rebindings", alias) {
                return false;
            }
            let mut imports = facts
                .references
                .iter()
                .filter(|reference| import_binds(reference, scope, alias));
            let Some(import) = imports.next() else {
                return false;
            };
            imports.next().is_none()
                && import.module.as_deref() == Some(module)
                && !matches!(import.receiver_hint, Some(ReceiverHint::TypeOnlyImport))
                && match member {
                    None => import.expression == *export,
                    Some(member) => import.expression == "*" && member == export,
                }
        }
        Api::ConstructedMember { .. } => false,
    }
}

pub(crate) fn scoped_relative_path(scope: &str, relative: &str) -> String {
    if scope.is_empty() {
        relative.to_owned()
    } else {
        format!("{scope}/{relative}")
    }
}

pub(crate) fn relative_under_directory<'a>(
    path: &'a str,
    project: &str,
    root: &str,
) -> Option<&'a str> {
    if project.is_empty() {
        path.strip_prefix(root)?.strip_prefix('/')
    } else {
        path.strip_prefix(project)?
            .strip_prefix('/')?
            .strip_prefix(root)?
            .strip_prefix('/')
    }
}

pub(crate) fn component_environment_config_path(
    rule: &super::syntax::SyntaxRule,
    path: &str,
    scope: &str,
    package: &str,
    config_file: &str,
    dependencies: &crate::languages::dependency_registry::DependencyRegistry,
) -> Option<String> {
    rule.activation_family()
        .is_some_and(|family| dependencies.declares_for_path(family, path, package))
        .then(|| scoped_relative_path(scope, config_file))
}

pub(crate) fn matching_component_configurations<'a>(
    configurations: impl IntoIterator<Item = &'a ComponentConfigurationFact>,
    rule: &'a super::syntax::SyntaxRule,
) -> impl Iterator<Item = &'a ComponentConfigurationFact> {
    configurations
        .into_iter()
        .filter(move |fact| fact.origin.pack == rule.pack && fact.origin.rule == rule.id)
}

pub(crate) fn component_export_matches_provider(
    provider_path: &str,
    scope: &str,
    directory: &str,
    extension: &str,
    dependencies: &crate::languages::dependency_registry::DependencyRegistry,
) -> bool {
    dependencies.nearest_manifest_scope_for_path(
        crate::languages::LanguageFamily("javascript"),
        provider_path,
    ) == Some(scope)
        && relative_under_directory(provider_path, scope, directory)
            .is_some_and(|tail| !tail.contains('/') && tail.ends_with(extension))
}

pub(crate) fn active_component_module_additions<'a>(
    configuration: &'a ComponentConfigurationFact,
    modules: &'a std::collections::BTreeMap<String, super::syntax::ComponentModule>,
    rule: &'a super::syntax::SyntaxRule,
    path: &'a str,
    dependencies: &'a crate::languages::dependency_registry::DependencyRegistry,
) -> impl Iterator<Item = (&'a str, &'a super::syntax::ComponentModule)> {
    configuration.modules.iter().filter_map(move |name| {
        let addition = modules.get(name).filter(|addition| {
            rule.activation_family()
                .is_some_and(|family| dependencies.declares_for_path(family, path, &addition.package))
        })?;
        Some((name.as_str(), addition))
    })
}
