use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticTarget {
    ProjectPathSuffix { prefix: String, language: String },
    ImportedSymbol { language: String },
    PythonSymbol { language: String },
}

impl SemanticTarget {
    pub fn paths(&self, path: &str, language: &str) -> Vec<String> {
        let Self::ProjectPathSuffix {
            prefix,
            language: expected,
        } = self
        else {
            return Vec::new();
        };
        if language != expected || !prefix.strip_suffix('/').is_some_and(normal_relative_path) {
            return Vec::new();
        }
        std::iter::once(0)
            .chain(path.match_indices('/').map(|(offset, _)| offset + 1))
            .filter_map(|offset| path[offset..].strip_prefix(prefix))
            .filter(|literal| normal_relative_path(literal))
            .map(str::to_owned)
            .collect()
    }
}

fn normal_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '\\' | ':' | '?' | '#'))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRelation {
    RendersTemplate,
    InjectsDependency,
    UsesModel,
    References,
}

impl SemanticRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RendersTemplate => "renders_template",
            Self::InjectsDependency => "injects_dependency",
            Self::UsesModel => "uses_model",
            Self::References => "references",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticIdentity {
    pub scope: String,
    pub namespace: String,
    pub target: SemanticTarget,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticOccurrence {
    pub owner_id: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticFactRole {
    FileProvider {
        target: String,
    },
    CallToFile {
        import_owner: String,
        alias: String,
        module: String,
        export: String,
        relation: SemanticRelation,
    },
    ClassAttributeToFile {
        attribute: String,
        relation: SemanticRelation,
    },
    ClassAttributeToSymbol {
        attribute: String,
        relation: SemanticRelation,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticFact {
    pub pack: String,
    pub rule: String,
    pub identity: SemanticIdentity,
    pub occurrence: SemanticOccurrence,
    pub expression: String,
    pub role: SemanticFactRole,
}
