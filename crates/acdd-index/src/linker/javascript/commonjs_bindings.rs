use crate::models::Node;
use crate::semantic::SourcePosition;
use hashbrown::HashMap;
use serde_json::Value;

#[derive(Clone, Copy)]
struct ResolvedCommonJsBinding {
    completed: SourcePosition,
    captured_safe: bool,
}

#[derive(Default)]
/// JavaScript-family CommonJS binding index prepared before linking.
pub(crate) struct ResolvedCommonJsBindings<'a> {
    bindings: HashMap<(&'a str, &'a str), Option<ResolvedCommonJsBinding>>,
}

fn position(value: &Value) -> Option<SourcePosition> {
    let object = value.as_object()?;
    if object.len() != 2 {
        return None;
    }
    Some(SourcePosition {
        line: usize::try_from(object.get("line")?.as_u64()?).ok()?,
        column: usize::try_from(object.get("column")?.as_u64()?).ok()?,
    })
}

impl<'a> ResolvedCommonJsBindings<'a> {
    pub(crate) fn build(all: &'a dyn crate::FactsLookup) -> Self {
        let mut result = Self::default();
        for facts in all.values_facts() {
            for owner in facts.nodes.iter().filter(|node| {
                node.kind == crate::models::tags::NodeKind::Module
                    && matches!(node.language.as_str(), "javascript" | "typescript" | "vue")
            }) {
                let Some(records) = owner.detail_entry(crate::models::details::NodeDetailKey::COMMONJS_BINDINGS).as_array() else {
                    continue;
                };
                if records.is_empty() {
                    continue;
                }
                let mut imports = HashMap::new();
                for reference in facts
                    .references
                    .iter()
                    .filter(|reference| reference.kind == "imports" && reference.source == owner.id)
                {
                    if let Some(alias) = reference.alias.as_deref() {
                        imports
                            .entry(alias)
                            .and_modify(|value| *value = None)
                            .or_insert(Some(reference));
                    }
                }
                for record in records {
                    let Some(alias) = record["alias"].as_str() else {
                        continue;
                    };
                    let key = (owner.id.as_str(), alias);
                    let binding = (|| {
                        if record.as_object()?.len() != 6 || record["invalidated"].as_bool()? {
                            return None;
                        }
                        let module = record["module"].as_str()?;
                        let declaration = position(&record["declaration"])?;
                        let completed = position(&record["completed"])?;
                        if declaration >= completed {
                            return None;
                        }
                        let import = imports.get(alias).copied().flatten()?;
                        if import.module.as_deref() != Some(module)
                            || declaration != import.position()
                        {
                            return None;
                        }
                        Some(ResolvedCommonJsBinding {
                            completed,
                            captured_safe: record["captured_safe"].as_bool()?,
                        })
                    })();
                    result
                        .bindings
                        .entry(key)
                        .and_modify(|value| *value = None)
                        .or_insert(binding);
                }
            }
        }
        result
    }

    pub(crate) fn tracks(&self, scope: &Node, alias: &str) -> bool {
        self.bindings.contains_key(&(scope.id.as_str(), alias))
    }

    pub(crate) fn admits(
        &self,
        scope: &Node,
        alias: &str,
        owner: &Node,
        at: SourcePosition,
    ) -> bool {
        if scope.kind != crate::models::tags::NodeKind::Module
            || scope.path != owner.path
            || !matches!(scope.language.as_str(), "javascript" | "typescript" | "vue")
        {
            return false;
        }
        let Some(Some(binding)) = self.bindings.get(&(scope.id.as_str(), alias)) else {
            return false;
        };
        at >= binding.completed && (scope.id == owner.id || binding.captured_safe)
    }
}

impl crate::languages::profile::LanguageImportState for ResolvedCommonJsBindings<'_> {
    fn tracks_binding(&self, scope: &Node, alias: &str) -> bool {
        self.tracks(scope, alias)
    }

    fn admits_binding(
        &self,
        scope: &Node,
        alias: &str,
        owner: &Node,
        at: SourcePosition,
    ) -> bool {
        self.admits(scope, alias, owner, at)
    }
}
