//! Language-dispatched incremental scope analysis state for single-pass AST DFS.

use super::syntax::{ScopeBindings, ScopeVisitor, Syntax};
use crate::languages::{
    support::value_flow_core::GenericScopeAnalysis, types::CompiledSyntaxGrammar,
};
use std::{collections::HashSet, marker::PhantomData, sync::Arc};

enum LanguageAnalysis<'source, 'tree> {
    #[cfg(feature = "lang-python")]
    Python(Box<crate::languages::python::value_flow::PythonValueFlowCollector<'source, 'tree>>),
    #[cfg(feature = "lang-rust")]
    Rust(
        Box<crate::languages::rust::value_flow::collector::RustValueFlowCollector<'source, 'tree>>,
    ),
    #[cfg(feature = "lang-typescript")]
    TypeScript(
        Box<crate::languages::typescript::value_flow::TypeScriptValueFlowCollector<'source, 'tree>>,
    ),
    Generic(Box<GenericScopeAnalysis<'source, 'tree>>),
    #[cfg(test)]
    Traced {
        inner: Box<ScopeAnalysis<'source, 'tree>>,
        on_finish: Box<dyn FnOnce()>,
    },
    Marker(PhantomData<(&'source str, Syntax<'tree>)>),
}

/// Incremental language body analysis driven exclusively by shared traversal events.
pub struct ScopeAnalysis<'source, 'tree> {
    state: Option<LanguageAnalysis<'source, 'tree>>,
    marker: PhantomData<(&'source str, Syntax<'tree>)>,
}

impl Default for ScopeAnalysis<'_, '_> {
    fn default() -> Self {
        Self {
            state: Some(LanguageAnalysis::Marker(PhantomData)),
            marker: PhantomData,
        }
    }
}

macro_rules! dispatch_visitor_mut {
    ($self:expr, $visitor:ident => $body:expr) => {
        match $self.state.as_mut() {
            #[cfg(feature = "lang-python")]
            Some(LanguageAnalysis::Python($visitor)) => $body,
            #[cfg(feature = "lang-rust")]
            Some(LanguageAnalysis::Rust($visitor)) => $body,
            #[cfg(feature = "lang-typescript")]
            Some(LanguageAnalysis::TypeScript($visitor)) => $body,
            Some(LanguageAnalysis::Generic($visitor)) => $body,
            #[cfg(test)]
            Some(LanguageAnalysis::Traced {
                inner: $visitor, ..
            }) => $body,
            Some(LanguageAnalysis::Marker(_)) | None => {}
        }
    };
}

impl<'source, 'tree> ScopeAnalysis<'source, 'tree> {
    #[cfg(feature = "lang-python")]
    pub(crate) fn python(root: Syntax<'tree>, source: &'source str) -> Self {
        Self {
            state: Some(LanguageAnalysis::Python(Box::new(
                crate::languages::python::value_flow::PythonValueFlowCollector::new(
                    root,
                    source,
                    root.kind() == "module",
                ),
            ))),
            marker: PhantomData,
        }
    }

    #[cfg(feature = "lang-rust")]
    pub(crate) fn rust(root: Syntax<'tree>, source: &'source str) -> Self {
        Self {
            state: Some(LanguageAnalysis::Rust(Box::new(
                crate::languages::rust::value_flow::collector::RustValueFlowCollector::new(
                    root, source,
                ),
            ))),
            marker: PhantomData,
        }
    }

    #[cfg(feature = "lang-typescript")]
    pub(crate) fn typescript(root: Syntax<'tree>, source: &'source str) -> Self {
        Self {
            state: Some(LanguageAnalysis::TypeScript(Box::new(
                crate::languages::typescript::value_flow::TypeScriptValueFlowCollector::new(
                    root, source,
                ),
            ))),
            marker: PhantomData,
        }
    }

    pub(crate) fn generic(
        root: Syntax<'tree>,
        source: &'source str,
        compiled: Arc<CompiledSyntaxGrammar>,
    ) -> Self {
        Self {
            state: Some(LanguageAnalysis::Generic(Box::new(
                GenericScopeAnalysis::new(root, source, compiled),
            ))),
            marker: PhantomData,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_finish_hook(self, on_finish: impl FnOnce() + 'static) -> Self {
        Self {
            state: Some(LanguageAnalysis::Traced {
                inner: Box::new(self),
                on_finish: Box::new(on_finish),
            }),
            marker: PhantomData,
        }
    }

    pub fn enter_with_kind(&mut self, node: Syntax<'tree>, kind: &str) {
        dispatch_visitor_mut!(self, visitor => visitor.enter_with_kind(node, kind));
    }

    pub fn observe_module_event(&mut self, node: Syntax<'tree>, entering: bool) {
        dispatch_visitor_mut!(self, visitor => visitor.observe_module_event(node, entering));
    }

    pub fn module_exports(
        &self,
        bindings: &ScopeBindings,
    ) -> Option<(Vec<crate::semantic::ExportBinding>, serde_json::Value)> {
        match self.state.as_ref() {
            #[cfg(feature = "lang-python")]
            Some(LanguageAnalysis::Python(visitor)) => {
                ScopeVisitor::module_exports(&**visitor, bindings)
            }
            #[cfg(feature = "lang-rust")]
            Some(LanguageAnalysis::Rust(visitor)) => visitor.module_exports(bindings),
            #[cfg(feature = "lang-typescript")]
            Some(LanguageAnalysis::TypeScript(visitor)) => visitor.module_exports(bindings),
            Some(LanguageAnalysis::Generic(visitor)) => visitor.module_exports(bindings),
            #[cfg(test)]
            Some(LanguageAnalysis::Traced { inner, .. }) => inner.module_exports(bindings),
            Some(LanguageAnalysis::Marker(_)) | None => None,
        }
    }

    pub fn exit(&mut self, node: Syntax<'tree>) {
        dispatch_visitor_mut!(self, visitor => visitor.exit(node));
    }

    pub fn enter_suspended(&mut self, node: Syntax<'tree>) {
        dispatch_visitor_mut!(self, visitor => visitor.enter_suspended(node));
    }

    pub fn exit_suspended(&mut self, node: Syntax<'tree>) {
        dispatch_visitor_mut!(self, visitor => visitor.exit_suspended(node));
    }

    pub fn finish(
        self,
        collect_module_aliases: bool,
    ) -> (crate::semantic::ValueFlowFacts, Option<HashSet<String>>) {
        match self.state {
            #[cfg(feature = "lang-python")]
            Some(LanguageAnalysis::Python(visitor)) => visitor.finish(collect_module_aliases),
            #[cfg(feature = "lang-rust")]
            Some(LanguageAnalysis::Rust(visitor)) => visitor.finish(collect_module_aliases),
            #[cfg(feature = "lang-typescript")]
            Some(LanguageAnalysis::TypeScript(visitor)) => visitor.finish(collect_module_aliases),
            Some(LanguageAnalysis::Generic(visitor)) => visitor.finish(collect_module_aliases),
            #[cfg(test)]
            Some(LanguageAnalysis::Traced { inner, on_finish }) => {
                let result = inner.finish(collect_module_aliases);
                on_finish();
                result
            }
            Some(LanguageAnalysis::Marker(_)) | None => (Default::default(), None),
        }
    }
}
