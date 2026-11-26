//! Core traits implemented by compiled language profiles and their linkers.

use crate::ast::{
    scope_analysis::ScopeAnalysis,
    syntax::{doc_comment, field, scope_bindings, symbol_name, text, ScopeBindings},
};
use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::{
    context::{publish_scope_projections, FileContext, LanguageContext, RuleIRHandle},
    dependency_registry::{parser::ModulePrefix, DependencyRegistry},
    routes::RouteContext,
    types::{
        self as languages, module_stem, AstSearchCapabilities, ImportPath, LanguageFamily,
        SymbolMetadata, SyntaxContext, SyntaxGrammarSpec,
    },
};
use crate::linker::{
    types::{BuiltinPropertyLookup, ModulesByNamespace, NodeBindingIndex, PackageExports},
    value_flow::types::{ReadTrace, SemanticResolver, TypeTarget, ValueFlowLookup},
};
use crate::models::{
    tags::{EdgeKind, NodeKind},
    Facts, FactsLookup, Node, Reference,
};
use anyhow::Result;
use hashbrown::HashMap;
use std::{
    collections::{BTreeSet, HashMap as StdHashMap},
    path::Path,
    sync::Arc,
};
use tree_sitter::{Node as Syntax, Parser};

/// Input for a language linker resolving one import.
pub struct ImportContext<'ctx, 'a, 'input> {
    /// Workspace-relative importing file.
    pub path: &'input str,
    /// Canonical symbol and path identities shared by linker indexes.
    pub interner: &'ctx LinkerInterner<'a>,
    /// Dependency data and framework manifests admitted for the indexed workspace.
    pub dependencies: &'ctx DependencyRegistry,
    /// Opaque language-owned state used for visibility and path resolution.
    pub language_state: Option<&'a (dyn LanguageImportState + 'a)>,
    /// Parsed import reference.
    pub reference: &'input Reference,
    /// Normalized import path from the language profile.
    pub normalized: Option<&'input ImportPath>,
    /// Namespace prefix that selected the indexed module.
    pub selected_namespace: &'input str,
    /// Language family of the importing file.
    pub family: LanguageFamily,
    /// Candidate modules for the import.
    pub modules: &'ctx mut Vec<&'a Node>,
    /// Candidate symbols selected by the linker.
    pub candidates: &'ctx mut Vec<&'a Node>,
    /// Modules grouped under language-specific import namespaces.
    pub modules_by_namespace: &'ctx ModulesByNamespace<'a>,
    /// Nodes grouped by source file.
    pub by_module: &'ctx HashMap<&'a str, Vec<&'a Node>>,
    /// Nodes grouped by fully qualified name.
    pub by_qual: &'ctx HashMap<&'a str, Vec<&'a Node>>,
    /// Explicit package re-exports discovered for the workspace.
    pub package_exports: &'ctx PackageExports<'a>,
    /// Reusable buffer for qualified-name lookup.
    pub lookup_key: &'ctx mut String,
}

/// Opaque per-language import resolver state shared by import and semantic resolution hooks.
pub trait LanguageImportState: Send + Sync {
    fn module_path_visible(&self, _namespace: &str, _requester: &str) -> bool {
        true
    }

    fn resolve_path<'a>(
        &'a self,
        _module: &Node,
        _tail: &str,
        _requester: &str,
        _by_qual: Option<&HashMap<&'a str, Vec<&'a Node>>>,
    ) -> Vec<&'a Node> {
        Vec::new()
    }

    fn external_binding_visible(
        &self,
        _provider: &Node,
        _binding_name: &str,
        _requester: &str,
        _direct_in_provider: bool,
    ) -> bool {
        true
    }

    fn targets(&self, _module: &Node, _name: &str, _requester: &str) -> &[&Node] {
        &[]
    }

    fn visible_bindings(&self, _module: &Node, _requester: &str) -> Vec<(&str, &[&Node])> {
        Vec::new()
    }

    fn workspace_crate_root(
        &self,
        _dependencies: &DependencyRegistry,
        _path: &str,
        _crate_name: &str,
    ) -> Option<&Node> {
        None
    }

    fn tracks_binding(&self, _scope: &Node, _alias: &str) -> bool {
        false
    }

    fn admits_binding(
        &self,
        _scope: &Node,
        _alias: &str,
        _owner: &Node,
        _at: crate::semantic::SourcePosition,
    ) -> bool {
        true
    }
}

impl ImportContext<'_, '_, '_> {
    /// Returns the imported symbol suffix, or an empty suffix for a module import.
    pub fn tail(&self) -> Option<&str> {
        let normalized = self.normalized?;
        Some(if normalized.symbol_path {
            normalized
                .namespace
                .strip_prefix(self.selected_namespace)
                .unwrap_or("")
                .trim_start_matches('.')
        } else if self.reference.expression == "*"
            || self.reference.module.as_deref() == Some(&self.reference.expression)
        {
            ""
        } else {
            &self.reference.expression
        })
    }
}

/// Language-specific import-resolution details returned to the common graph builder.
#[derive(Default)]
pub struct ImportResolution<'a> {
    /// Paired declaration-only module used when the runtime module lacks a symbol.
    pub paired_stub: Option<&'a Node>,
    /// The selected import names a child module rather than a declaration.
    pub child_module: bool,
    /// The selected declaration comes from a paired stub.
    pub stub_symbol: bool,
    /// The selected declaration is re-exported through a package initializer.
    pub reexport_symbol: bool,
}

/// Shared workspace inputs passed to [`LanguageLinker::normalize_file_imports`].
pub struct ImportNormalizationContext<'ctx, 'a> {
    /// Optional workspace root for root-relative import resolution.
    pub root: Option<&'ctx Path>,
    /// Workspace dependency and manifest registry.
    pub dependencies: &'ctx DependencyRegistry,
    /// Indexed source profiles keyed by workspace-relative path.
    pub profiles: &'ctx HashMap<&'a str, &'static dyn LanguageProfile>,
    /// Indexed module stems keyed by [`LanguageFamily`].
    pub family_module_stems: &'ctx HashMap<LanguageFamily, std::collections::HashSet<&'a str>>,
}

/// Borrowed inputs passed to [`LanguageLinker::compose_file_relations`].
pub(crate) struct FileRelationContext<'ctx, 'a> {
    /// Workspace-relative file path.
    pub path: &'a str,
    /// Extracted file facts.
    pub facts: &'a Facts,
    /// Catalog-restored or file-local references.
    pub references: &'ctx [Reference],
    /// Workspace dependency registry.
    pub dependencies: &'ctx DependencyRegistry,
    /// Shared semantic context.
    pub semantic_context: &'ctx crate::linker::semantic_context::context::Context<'a>,
    /// Nodes grouped by file path.
    pub by_module: &'ctx HashMap<&'a str, Vec<&'a Node>>,
    /// Nodes grouped by interned symbol ID.
    pub by_id: &'ctx HashMap<SymbolId, &'a Node>,
    /// Prepared framework semantic facts.
    pub prepared_semantic_facts: &'ctx crate::linker::semantic_facts::PreparedSemanticFacts<'a>,
    /// Mutable per-file output graph.
    pub graph: &'ctx mut crate::models::compact_graph::CompactGraph,
}

/// Owns language-specific import, receiver, and incremental-linking rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanguageValueProjection {
    GenericCall,
    Array,
    Tuple,
    BranchJoin,
    Reference { mutable: bool },
    Field(String),
    Adapter,
}

/// Language-owned, borrowed index of members for one or more compiled profiles.
pub trait LanguageMemberIndex: Send + Sync {
    /// Returns every language profile ID served by this member index.
    fn language_ids(&self) -> &'static [&'static str];

    /// Reports whether this index owns local-member lookup for a receiver.
    fn handles_receiver(&self, _receiver: &Node) -> bool {
        true
    }

    /// Looks up an ordinary local receiver without changing explicit super semantics.
    fn lookup_local_member<'a>(
        &'a self,
        receiver: &Node,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> LanguageMemberResult<'a> {
        self.lookup_member(receiver, member, associated, caller)
    }

    /// Looks up a local, trait, or framework member on `receiver`.
    fn lookup_member<'a>(
        &'a self,
        _receiver: &Node,
        _member: &str,
        _associated: bool,
        _caller: &Node,
    ) -> LanguageMemberResult<'a> {
        LanguageMemberResult::Unknown
    }

    /// Looks up a property/field declaration on `receiver`.
    fn lookup_property<'a>(&'a self, _receiver: &Node, _member: &str) -> LanguageMemberResult<'a> {
        LanguageMemberResult::Unknown
    }

    /// Looks up a member on a built-in/primitive receiver category.
    fn lookup_builtin<'a>(
        &'a self,
        _receiver: &str,
        _member: &str,
        _associated: bool,
        _caller: &Node,
    ) -> LanguageMemberResult<'a> {
        LanguageMemberResult::Unknown
    }

    /// Returns indexed concrete implementors for a trait or interface declaration.
    fn implementors(&self, _base: &Node) -> Option<&[&Node]> {
        None
    }

    /// Returns the `impl` / struct receiver node associated with `method`.
    fn receiver_for(&self, _method: &Node) -> Option<&Node> {
        None
    }

    /// Returns the `Deref::Target` node registered for `receiver`.
    fn deref_target(&self, _receiver: &Node) -> Option<&Node> {
        None
    }

    /// Reports whether `method` is an inherent method rather than a trait impl member.
    fn is_inherent_member(&self, _method: &Node) -> bool {
        false
    }

    /// Reports whether `receiver` implements `Display` / `ToString`.
    fn builtin_local_member(&self, _receiver: &Node, _member: &str, _associated: bool) -> bool {
        false
    }

    /// Returns a configured hierarchy return policy for `receiver.member`.
    fn return_policy(
        &self,
        _receiver: &Node,
        _member: &str,
    ) -> Option<(
        &str,
        &crate::languages::manifests::ClassHierarchyReceiverRule,
        &crate::languages::manifests::ReturnPolicy,
    )> {
        None
    }

    /// Returns dynamic dispatch `(trait_method, implementors)` candidates for `receiver.member`.
    fn dynamic_dispatch(&self, _receiver: &Node, _member: &str) -> Option<(&Node, Vec<&Node>)> {
        None
    }
}

/// Shared evaluation state passed to [`LanguageLinker`] value-flow hooks.
pub struct CallEvalContext<'c, 'a, 't> {
    /// Read-only view into scopes, fields, and returns computed so far.
    pub index: &'c (dyn ValueFlowLookup<'a> + 'a),
    /// Enclosing declaration whose body contains the expression.
    pub owner: &'a Node,
    /// Source coordinates of the call or projection site.
    pub at: crate::semantic::SourcePosition,
    /// Shared symbol and configured-rule resolver.
    pub resolver: &'c dyn SemanticResolver<'a>,
    /// Dependency epoch recorder for memoized fixed-point re-evaluation.
    pub trace: Option<&'c ReadTrace<'a, 't>>,
}

/// Result of a language member-index query, independent of any profile's storage shape.
pub enum LanguageMemberResult<'a> {
    Local(&'a Node),
    Candidates(Vec<&'a Node>),
    DynamicDispatch(&'a Node, Vec<&'a Node>),
    External {
        module: &'a str,
        import_line: usize,
        provider: Option<&'a str>,
        hierarchy: Option<(
            &'a str,
            &'a crate::languages::manifests::ClassHierarchyReceiverRule,
        )>,
    },
    Ambiguous,
    Unknown,
}

impl<'a> LanguageMemberResult<'a> {
    /// Collects local candidate declarations from this member lookup result.
    pub fn into_nodes(self) -> Vec<&'a Node> {
        match self {
            Self::Local(node) => vec![node],
            Self::Candidates(nodes) => nodes,
            _ => Vec::new(),
        }
    }
}

pub(crate) trait LanguageLinker: Send + Sync {
    fn reference_extension_factory<'a>(
        &self,
        _input: &crate::linker::traits::ReferenceWorkspace<'a>,
    ) -> Option<Box<dyn crate::linker::traits::ReferenceExtensionFactory<'a> + 'a>> {
        None
    }
    /// Normalizes a method's lexical owner scope for member lookup.
    fn member_owner_scope<'a>(&self, scope: &'a str) -> &'a str {
        scope
    }
    /// Whether a declaration may own language-specific associated members.
    fn associated_member_owner(&self, _node: &Node) -> bool {
        false
    }
    /// Resolves a member reached through an explicit constant address.
    fn explicit_constant_member<'a>(
        &self,
        _constant: &'a Node,
        _member: &str,
        _owner: &'a Node,
        _resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) -> Option<&'a Node> {
        None
    }
    /// Finds declarations reachable through an explicit module reexport.
    fn explicit_reexport_targets<'a>(
        &self,
        _query: crate::linker::traits::ExplicitReexport<'a, '_>,
    ) -> Vec<&'a Node> {
        Vec::new()
    }
    /// Whether the node owns module-level field facts.
    fn owns_module_fields(&self, _node: &Node) -> bool {
        false
    }
    /// Resolves a language-specific implementation source when applicable.
    fn implementation_source(
        &self,
        _input: crate::linker::traits::ImplementationSource<'_, '_>,
    ) -> Option<(Option<String>, bool)> {
        None
    }
    /// Whether a macro reference may point to this candidate.
    fn macro_reference_candidate(&self, _owner: Option<&Node>, _candidate: &Node) -> bool {
        false
    }
    /// Records language-specific gaps after candidate selection.
    fn record_candidate_gaps(
        &self,
        _profile: &dyn LanguageProfile,
        _reference: &Reference,
        _candidates: &[&Node],
        _path: &str,
        _graph: &mut crate::models::compact_graph::CompactGraph,
    ) {
    }
    /// Whether a static receiver expression can target this declaration.
    fn admits_static_receiver_candidate(&self, _candidate: &Node, _expression: &str) -> bool {
        true
    }
    /// Whether a class is a language-specific protocol or abstract base.
    fn has_protocol_or_abc_base(&self, _candidate: &Node) -> bool {
        false
    }
    /// Whether references first resolve through property lookup.
    fn property_before_member(&self) -> bool {
        false
    }
    /// Whether an unresolved reference falls back to property lookup.
    fn property_after_unknown(&self, _profile: &dyn LanguageProfile) -> bool {
        false
    }
    /// Whether a reference is a component template member.
    fn is_component_template_member(
        &self,
        _reference: &Reference,
        _owner: Option<&Node>,
        _tail: &str,
    ) -> bool {
        false
    }
    /// Language-specific names for invoking class objects and instances.
    fn callable_members(&self) -> Option<(&'static str, &'static str)> {
        None
    }
    /// Coverage evidence for a typed callable binding.
    fn typed_callable_evidence(&self) -> &'static str {
        "invocation of a typed Callable binding"
    }

    /// Whether class objects use associated local-member lookup.
    fn class_type_uses_associated_lookup(&self) -> bool {
        false
    }

    /// Whether collection flow facts are admitted for this language.
    fn supports_collection_facts(&self) -> bool {
        false
    }

    /// Whether an owner's async return participates in value flow.
    fn supports_async_return(&self, _owner: &Node) -> bool {
        false
    }

    fn normalize_builtin_receiver<'a>(&self, receiver: &'a str) -> &'a str {
        receiver
    }

    fn external_member_policy<'a>(
        &self,
        _owner: &Node,
        _module: &'a str,
        _line: usize,
        _member: &str,
    ) -> Option<LanguageMemberResult<'a>> {
        None
    }
    fn framework_member_label(&self) -> &'static str {
        "finite framework member"
    }
    fn wrapper_value_member(&self, _wrapper: &str, _member: &str) -> bool {
        false
    }
    fn standard_external_member(&self, _module: &str, _member: &str) -> bool {
        false
    }
    fn resolve_applied_type<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _owner: &'a Node,
        _application: (&str, &[crate::semantic::TypeExpr]),
        _nominal: &crate::linker::value_flow::Symbol<'a>,
        _at: crate::semantic::SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn admits_type_payload<'a>(
        &self,
        _owner: &Node,
        _wrapper: &TypeTarget<'a>,
        _count: usize,
    ) -> bool {
        false
    }
    fn external_generic_base(
        &self,
        _module: &str,
        _base: &str,
        _profile: &dyn LanguageProfile,
    ) -> bool {
        false
    }
    fn external_factory_result<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _owner: &'a Node,
        _callee: &str,
        _at: crate::semantic::SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn receiver_access(&self, _owner: &Node, _head: &str) -> Option<(bool, bool)> {
        None
    }
    fn special_property_lookup<'s, 'a>(
        &self,
        _context: &'s crate::linker::semantic_context::context::Context<'a>,
        _receiver: &'s TypeTarget<'a>,
        _member: &str,
        _caller: &'a Node,
    ) -> Option<crate::linker::semantic_context::context::Member<'s>> {
        None
    }
    fn special_member_result<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _receiver: &TypeTarget<'a>,
        _member: &str,
        _owner: &'a Node,
    ) -> Option<Option<TypeTarget<'a>>> {
        None
    }
    fn member_receiver<'b, 'a>(
        &self,
        _receiver: &'b TypeTarget<'a>,
        _member: &str,
    ) -> Option<Option<&'b TypeTarget<'a>>> {
        None
    }
    fn imported_member_path<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _owner: &'a Node,
        _targets: &[&'a Node],
        _member: &str,
        _role: crate::linker::value_flow::SymbolRole,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        None
    }
    fn external_symbol<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _owner: &'a Node,
        _name: &str,
        _role: crate::linker::value_flow::SymbolRole,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        None
    }
    fn special_symbol<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _owner: &'a Node,
        _name: &str,
        _at: crate::semantic::SourcePosition,
        _role: crate::linker::value_flow::SymbolRole,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        None
    }
    fn special_named_return<'a>(
        &self,
        _name: &str,
        _import_line: usize,
        _in_hierarchy: &dyn Fn() -> bool,
    ) -> Option<Option<TypeTarget<'a>>> {
        None
    }
    fn special_member_lookup<'s, 'a>(
        &self,
        _context: &'s crate::linker::semantic_context::context::Context<'a>,
        _receiver: &'s TypeTarget<'a>,
        _member: &str,
        _caller: &'a Node,
        _associated: bool,
    ) -> Option<crate::linker::semantic_context::context::Member<'s>> {
        None
    }
    fn classic_global_scope(&self, _node: &Node) -> bool {
        false
    }
    fn tracks_factory_results(&self) -> bool {
        false
    }
    fn uses_module_exports(&self) -> bool {
        false
    }
    fn structural_operation_admitted<'a>(
        &self,
        _owner: &'a Node,
        _operation: crate::semantic::types::RustProjection,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> bool {
        false
    }
    fn lexical_receiver_field(&self, _owner: &Node, _receiver: &str) -> bool {
        false
    }
    fn value_shadows_role(
        &self,
        _name: &str,
        _role: crate::linker::value_flow::SymbolRole,
    ) -> bool {
        true
    }
    fn lexical_binding_active(
        &self,
        _reference: &Reference,
        _binding_owner: &Node,
        _flow: Option<&crate::typed_facts::FlowState>,
        _first: &str,
    ) -> bool {
        true
    }
    fn external_field_projection(&self) -> bool {
        false
    }
    fn builtin_field_projection(&self, _owner: &Node) -> bool {
        false
    }
    fn payload_uses_async_return(&self) -> bool {
        false
    }
    fn reaching_payload_wrapper(&self, target: &TypeTarget<'_>) -> bool {
        matches!(target, TypeTarget::Promise(_))
    }
    fn join_language_targets<'a>(
        &self,
        _targets: &[TypeTarget<'a>],
        _join: &dyn Fn(Vec<TypeTarget<'a>>) -> TypeTarget<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn evaluate_language_value<'a>(
        &self,
        _eval: &CallEvalContext<'_, 'a, '_>,
        _value: &crate::semantic::ValueExpr,
        _limits: crate::linker::value_flow::SemanticLimits,
        _evaluate: &dyn Fn(&crate::semantic::ValueExpr) -> TypeTarget<'a>,
        _payload: &dyn Fn(&crate::semantic::ValueExpr) -> TypeTarget<'a>,
        _join: &dyn Fn(Vec<TypeTarget<'a>>) -> TypeTarget<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn special_index_projection<'a>(
        &self,
        _target: &TypeTarget<'a>,
        _index: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn special_field_projection<'a>(
        &self,
        _target: &TypeTarget<'a>,
        _member: &str,
        _owner: &'a Node,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn callback_receiver<'a>(
        &self,
        _receiver: &TypeTarget<'a>,
        _method: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn resolve_super_member<'a>(
        &self,
        _context: &crate::linker::semantic_context::context::Context<'a>,
        _owner: &Node,
        _member: &str,
    ) -> crate::linker::value_flow::Symbol<'a> {
        crate::linker::value_flow::Symbol::Unknown
    }
    fn incomplete_receiver(&self, _receiver: &TypeTarget<'_>) -> bool {
        false
    }
    fn special_member_call<'a>(
        &self,
        _eval: &CallEvalContext<'_, 'a, '_>,
        _receiver: &crate::semantic::ValueExpr,
        _member: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn accept_member_return(&self, _member: &str, _result: &TypeTarget<'_>) -> bool {
        true
    }
    fn member_call_result<'a>(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        at: crate::semantic::SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        resolver.resolve_member_result(receiver, member, owner, at)
    }
    fn indirect_call_result<'a>(
        &self,
        _eval: &CallEvalContext<'_, 'a, '_>,
        _callee: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn inherits_scope(&self, _node: &Node, _parent: &Node) -> bool {
        true
    }
    fn scoped_binding_restores(&self) -> bool {
        true
    }
    fn collection_return(&self, _target: &crate::linker::value_flow::TypeTarget<'_>) -> bool {
        false
    }
    fn commonjs_module(&self, _node: &Node) -> bool {
        false
    }
    fn component_default_export(&self, _node: &Node) -> bool {
        false
    }
    fn project_fields<'a>(
        &self,
        _fields: &mut HashMap<&'a str, HashMap<String, crate::linker::value_flow::TypeTarget<'a>>>,
        _owners: &[&'a Node],
        _resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) {
    }
    fn receiver_symbol<'a>(
        &self,
        _lookup: &crate::linker::semantic_context::context::LanguageMemberLookup<'_, 'a>,
        _owner: &'a Node,
        _name: &str,
        _resolve_type: &mut dyn FnMut(&str) -> crate::linker::value_flow::Symbol<'a>,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        None
    }
    fn builtin_type_symbol(
        &self,
        _owner: &Node,
        _profile: &dyn LanguageProfile,
        _name: &str,
        _role: crate::linker::value_flow::SymbolRole,
    ) -> bool {
        false
    }
    fn external_import_type<'a>(
        &self,
        _owner: &Node,
        _module: &str,
        _reference: &Reference,
        _names: (&str, &str),
        _role: crate::linker::value_flow::SymbolRole,
        _line: usize,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        None
    }
    fn project_reference<'a>(&self, target: TypeTarget<'a>, _mutable: bool) -> TypeTarget<'a> {
        target
    }
    fn project_tuple<'a>(&self, items: Vec<TypeTarget<'a>>) -> TypeTarget<'a> {
        TypeTarget::Tuple(items)
    }
    fn project_applied_return<'a>(
        &self,
        _name: &str,
        _first: &TypeTarget<'a>,
        _second: &mut dyn FnMut() -> Option<TypeTarget<'a>>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn collection_item<'a>(&self, _target: &TypeTarget<'a>) -> Option<TypeTarget<'a>> {
        None
    }
    fn generic_sequence_return(&self) -> bool {
        true
    }
    fn import_visible_before_position(&self, _owner: &Node) -> bool {
        false
    }
    fn builtin_type_members(&self) -> bool {
        false
    }
    fn implicit_receiver_evidence<'a>(
        &self,
        _first: &'a str,
        _tail: &'a str,
        _context: &LanguageContext,
        _path: &str,
        _signals: &crate::languages::manifests::semantic::FileActivationSignals,
    ) -> Option<(&'a str, String)> {
        None
    }
    fn is_implicit_self(&self, _first: &str) -> bool {
        false
    }
    fn builtin_property_chain(
        &self,
        _expression: &str,
        _profile: &dyn LanguageProfile,
        _path: &str,
        _context: &crate::linker::semantic_context::context::Context<'_>,
    ) -> bool {
        false
    }
    fn nullable_parameter_wrapper(
        &self,
        _profile: &dyn LanguageProfile,
        _annotation: &str,
        _node: &Node,
        _references: &[Reference],
        _external_line: &dyn Fn(&str, &Node) -> Option<usize>,
    ) -> bool {
        false
    }
    fn object_ownership<'a>(
        &self,
        _node: &Node,
        _flows: Option<&HashMap<SymbolId, &'a crate::typed_facts::FlowState>>,
        _interner: &LinkerInterner<'_>,
        _rules: &[crate::languages::manifests::syntax::SyntaxRule],
    ) -> Option<&'a crate::semantic::ObjectOwnershipFact> {
        None
    }
    fn lexical_this_calls<'a, 'b>(
        &self,
        _facts: &'a Facts,
        _references: &'b [Reference],
        _flows: Option<&HashMap<SymbolId, &'a crate::typed_facts::FlowState>>,
        _interner: &LinkerInterner<'_>,
        _rules: &[crate::languages::manifests::syntax::SyntaxRule],
    ) -> hashbrown::HashSet<(&'b str, usize, usize, &'b str)> {
        hashbrown::HashSet::new()
    }
    fn returned_object_field_writes<'a, 'b>(
        &self,
        _profile: &dyn LanguageProfile,
        _references: &'b [Reference],
        _by_id: &HashMap<SymbolId, &'a Node>,
        _interner: &LinkerInterner<'_>,
    ) -> hashbrown::HashSet<(&'a str, &'b str)> {
        hashbrown::HashSet::new()
    }
    fn reference_profile<'a>(
        &self,
        profile: &'a dyn LanguageProfile,
        _reference: &Reference,
        _by_id: &HashMap<SymbolId, &Node>,
        _interner: &LinkerInterner<'_>,
    ) -> &'a dyn LanguageProfile {
        profile
    }

    fn path_constructor_aliases<'a>(
        &self,
        _profile: &dyn LanguageProfile,
        _references: &'a [Reference],
        _by_id: &HashMap<SymbolId, &'a Node>,
        _interner: &LinkerInterner<'_>,
        _module: &'a str,
    ) -> HashMap<(&'a str, &'a str), &'a Reference> {
        HashMap::new()
    }
    fn dynamic_type_aliases<'a>(
        &self,
        _profile: &dyn LanguageProfile,
        _references: &'a [Reference],
        _by_id: &HashMap<SymbolId, &'a Node>,
        _interner: &LinkerInterner<'_>,
        _module: &'a str,
    ) -> HashMap<&'a str, hashbrown::HashSet<&'a str>> {
        HashMap::new()
    }

    /// Resolves the element type of a language-specific collection return.
    fn collection_return_element<'a>(
        &self,
        _owner: &'a Node,
        _expression: &crate::semantic::TypeExpr,
        _resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
        _limits: crate::linker::value_flow::SemanticLimits,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        None
    }
    /// Resolves an explicitly rooted module reference.
    fn explicit_module_target(
        &self,
        _profile: &dyn LanguageProfile,
        _phase: &crate::linker::traits::ReferencePhase<'_, '_>,
    ) -> Option<crate::linker::traits::ExplicitModuleTarget> {
        None
    }

    /// Resolves language-specific property references before generic admission.
    fn property_reference(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
    ) -> bool {
        false
    }

    /// Resolves language-specific references after generic admission.
    fn special_reference(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
    ) -> bool {
        false
    }

    fn dynamic_reference(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
    ) -> bool {
        false
    }

    fn early_reference(&self, _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>) -> bool {
        false
    }

    fn template_tag_reference(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
    ) -> bool {
        false
    }
    fn configured_template_reference(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
        _context: &crate::languages::LanguageContext,
        _signals: &crate::languages::manifests::semantic::FileActivationSignals,
    ) -> bool {
        false
    }
    fn macro_callable_origin(
        &self,
        _reference: &crate::models::Reference,
        _context: &crate::languages::LanguageContext,
    ) -> Option<String> {
        None
    }
    fn builtin_this_member(
        &self,
        _phase: &crate::linker::traits::ReferencePhase<'_, '_>,
        _expression: &str,
        _writes: &hashbrown::HashSet<(&str, &str)>,
    ) -> bool {
        false
    }
    fn lexical_this_member(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
        _expression: &str,
        _calls: &hashbrown::HashSet<(&str, usize, usize, &str)>,
    ) -> bool {
        false
    }
    fn object_reference(
        &self,
        _context: &mut crate::linker::traits::ObjectReference<'_, '_, '_>,
    ) -> bool {
        false
    }
    fn component_slot_reference(
        &self,
        _context: &mut crate::linker::traits::ComponentSlotReference<'_, '_>,
    ) -> bool {
        false
    }
    fn template_setup_binding<'a>(
        &self,
        _reference: &crate::models::Reference,
        _component: Option<&'a Node>,
    ) -> Option<&'a Node> {
        None
    }
    fn template_lexical_reference(
        &self,
        _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>,
        _first: &str,
        _component: Option<&Node>,
    ) -> bool {
        false
    }

    fn allows_typed_member_read(&self, _known_receiver: bool) -> bool {
        false
    }
    fn supports_constant_receiver(&self) -> bool {
        false
    }
    fn chained_member_fallback(&self) -> bool {
        false
    }
    fn supports_static_import_receiver(&self) -> bool {
        false
    }
    fn imported_module_first(&self) -> bool {
        false
    }
    fn explicit_module_visible(
        &self,
        _query: &crate::linker::traits::ExplicitModuleVisibility<'_, '_>,
    ) -> bool {
        true
    }
    fn framework_magic_diagnostic(&self, _profile: &dyn LanguageProfile, _module: &str) -> bool {
        false
    }
    fn external_builtin_evidence(
        &self,
        _profile: &dyn LanguageProfile,
        _module: &str,
        _expression: &str,
    ) -> Option<String> {
        None
    }
    fn preserves_external_import_line(&self) -> bool {
        false
    }
    fn associated_module_owner(&self, _node: &crate::models::Node) -> bool {
        false
    }
    fn admits_lexical_candidate(&self, _node: &Node) -> bool {
        true
    }
    fn import_visible_at(
        &self,
        _context: &crate::linker::traits::ImportVisibility<'_, '_, '_>,
        _reference: &crate::models::Reference,
        _definition_scope: bool,
    ) -> bool {
        true
    }
    fn implicit_receiver_member<'a>(
        &self,
        _query: &mut crate::linker::traits::ImplicitReceiver<'a, '_, '_>,
    ) -> Option<LanguageMemberResult<'a>> {
        None
    }
    fn computed_receiver<'a, 'b>(
        &self,
        _query: &mut crate::linker::traits::DynamicReceiver<'a, 'b, '_>,
    ) -> (LanguageMemberResult<'b>, bool)
    where
        'a: 'b,
    {
        (LanguageMemberResult::Unknown, false)
    }
    fn imported_callable<'a>(
        &self,
        _phase: &crate::linker::traits::ReferencePhase<'a, '_>,
        _targets: &[&'a Node],
        _member: &str,
    ) -> Vec<&'a Node> {
        Vec::new()
    }
    fn imported_external_member<'a>(
        &self,
        _phase: &crate::linker::traits::ReferencePhase<'a, '_>,
        _module: &'a Node,
        _member: &str,
        _lookup: &mut String,
    ) -> Option<crate::linker::traits::ExternalMember<'a>> {
        None
    }
    fn imported_default_callable<'a>(
        &self,
        _phase: &crate::linker::traits::ReferencePhase<'a, '_>,
        _first: &str,
        _facts: &crate::models::Facts,
        _imported: &[&'a Node],
        _exports: &crate::linker::traits::PackageExports<'a>,
        _lookup: &mut String,
    ) -> Option<&'a Node> {
        None
    }

    /// Whether an expression starts with an explicitly rooted module prefix.
    fn explicit_module_prefix(&self, _expression: &str) -> bool {
        false
    }

    /// Resolves template builtin evidence when this profile owns template syntax.
    fn template_builtin(
        &self,
        _expression: &str,
        _rules: &[&crate::languages::manifests::syntax::SyntaxRule],
    ) -> Option<String> {
        None
    }

    /// Resolves a platform builtin origin after configured rules are considered.
    fn platform_builtin_origin<'a>(
        &self,
        _profile: &dyn LanguageProfile,
        _first: &str,
        _configured: Option<&'a str>,
    ) -> Option<&'a str> {
        None
    }

    /// Classifies a language builtin and its reference evidence.
    fn builtin_evidence(&self, _first: &str, _expression: &str) -> Option<(&'static str, String)> {
        None
    }

    /// Reports an external annotation admitted by the language.
    fn external_annotation(
        &self,
        _profile: &dyn LanguageProfile,
        _head: &str,
        _path: &str,
        _dependencies: &DependencyRegistry,
    ) -> bool {
        false
    }

    /// Whether a constant can denote a callable value in this language.
    fn callable_constant(&self, _node: &Node) -> bool {
        false
    }

    /// Whether script-style optional chains and implicit receivers are supported.
    fn script_receiver_syntax(&self) -> bool {
        false
    }

    /// Default origin of builtins supplied by this language.
    fn builtin_origin(&self) -> Option<&'static str> {
        None
    }

    /// Whether a declaration denotes a variant whose result is its parent enum.
    fn returns_parent_enum(&self, _node: &Node) -> bool {
        false
    }

    /// Whether a member declaration is visible at a reference position.
    fn member_visible_at(&self, _node: &Node, _path: &str, _line: usize, _column: usize) -> bool {
        true
    }

    /// Whether a typed call-result reference can resolve as a computed member.
    fn admits_computed_reference(&self, _language: &str) -> bool {
        false
    }

    /// Whether shared class-member indexing applies to this language.
    fn uses_shared_class_members(&self) -> bool {
        true
    }

    /// Whether hierarchy manifest rules are used by this language.
    fn uses_hierarchy_rules(&self) -> bool {
        false
    }

    /// Resolves a member through this linker's opaque language-owned state.
    fn lookup_member<'a>(
        &self,
        indexes: &HashMap<&'static str, &'a (dyn LanguageMemberIndex + 'a)>,
        language_id: &str,
        receiver: &Node,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> LanguageMemberResult<'a> {
        indexes
            .get(language_id)
            .map_or(LanguageMemberResult::Unknown, |index| {
                index.lookup_member(receiver, member, associated, caller)
            })
    }

    fn lookup_builtin<'a>(
        &self,
        indexes: &HashMap<&'static str, &'a (dyn LanguageMemberIndex + 'a)>,
        language_id: &str,
        receiver: &str,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> LanguageMemberResult<'a> {
        indexes
            .get(language_id)
            .map_or(LanguageMemberResult::Unknown, |index| {
                index.lookup_builtin(receiver, member, associated, caller)
            })
    }

    /// Resolves a language-specific constructor or direct call expression before fallback expression lookup.
    fn evaluate_call_fallback<'a>(
        &self,
        _eval: &CallEvalContext<'_, 'a, '_>,
        _callee: &str,
        _site: Option<&crate::semantic::CallSiteEvidence>,
    ) -> Option<TypeTarget<'a>> {
        None
    }

    /// Resolves dotted/chained call targets after direct expression type lookup misses.
    fn evaluate_dotted_call<'a>(
        &self,
        _eval: &CallEvalContext<'_, 'a, '_>,
        _callee: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }

    /// Resolves platform or synthetic member-call return types when standard lookup finds none.
    fn evaluate_member_call_fallback<'a>(
        &self,
        _eval: &CallEvalContext<'_, 'a, '_>,
        _receiver: &TypeTarget<'a>,
        _callee: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }

    /// Resolves a language-owned synthetic property type for `receiver.member`.
    fn synthetic_property_result<'a>(
        &self,
        _owner: &'a Node,
        _receiver: &TypeTarget<'a>,
        _member: &str,
    ) -> Option<TypeTarget<'a>> {
        None
    }

    /// Extracts a local class/struct declaration from a resolved receiver type.
    fn local_receiver_class<'a>(&self, target: &TypeTarget<'a>) -> Option<&'a Node> {
        match target {
            TypeTarget::Local(node) => Some(*node),
            _ => None,
        }
    }

    /// Unwraps language reference wrappers (e.g., `&T` / `&mut T`) around a target type.
    fn unwrap_reference<'t, 'a>(&self, target: &'t TypeTarget<'a>) -> &'t TypeTarget<'a> {
        target
    }

    /// Resolves a language-specific constructor expression before lexical lookup.
    fn construct_value<'a>(&self, _owner: &'a Node, _callee: &str) -> Option<TypeTarget<'a>> {
        None
    }

    /// Resolves a language-specific constructor result using the shared semantic resolver.
    fn constructor_result<'a>(
        &self,
        _owner: &'a Node,
        _callee: &str,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn crate::linker::value_flow::SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }

    /// Identifies opaque callable marker values whose invocation is handled by this language.
    fn callable_marker_evidence(&self, _receiver: &TypeTarget<'_>) -> Option<&'static str> {
        None
    }

    /// Disambiguates candidate call/handler targets before language-specific gap diagnostics.
    fn filter_call_candidates(
        &self,
        _member_state: Option<&dyn LanguageMemberIndex>,
        _edge_kind: &str,
        _candidates: &mut Vec<&Node>,
    ) {
    }

    /// Reports whether a receiver member candidate is valid when invoked dynamically.
    fn admits_dynamic_member(
        &self,
        _member_state: Option<&dyn LanguageMemberIndex>,
        _dynamic: bool,
        _node: &Node,
    ) -> bool {
        true
    }

    /// Classifies an opaque language projection for the shared value-flow reducer.
    fn value_projection(&self, _member: &str) -> Option<LanguageValueProjection> {
        None
    }
    fn structural_value<'a>(
        &self,
        _target: &TypeTarget<'a>,
        _operation: crate::semantic::types::RustProjection,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn adapter_value<'a>(
        &self,
        _target: &TypeTarget<'a>,
        _member: &str,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn declared_adapter_value<'a>(
        &self,
        _target: &TypeTarget<'a>,
        _member: &str,
        _owner: &'a Node,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn deserialize_value<'a>(
        &self,
        _owner: &'a Node,
        _callee: &str,
        _payload: &crate::semantic::TypeExpr,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
        _limits: crate::linker::value_flow::SemanticLimits,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn is_value_adapter(&self, _member: &str) -> bool {
        false
    }
    fn value_payload<'a>(&self, _target: &TypeTarget<'a>) -> Option<TypeTarget<'a>> {
        None
    }

    /// Reports whether multi-file packages/namespaces are admitted as single-package imports.
    fn allows_multi_file_modules(&self) -> bool {
        false
    }

    /// Reports whether pre-indexed module stems for this profile's family are needed during import normalization.
    fn needs_family_module_stems(&self) -> bool {
        false
    }

    /// Normalizes all import references for a single file into `out`.
    fn normalize_file_imports<'a>(
        &self,
        profile: &dyn LanguageProfile,
        path: &'a str,
        facts: &'a Facts,
        ctx: &ImportNormalizationContext<'_, 'a>,
        out: &mut HashMap<(&'a str, &'a str), Option<ImportPath>>,
    ) {
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            let module = reference.module.as_deref().unwrap_or(&reference.expression);
            out.entry((path, module)).or_insert_with(|| {
                normalize_import_with_prefixes(profile, path, module, ctx.root, ctx.dependencies)
            });
        }
    }

    /// Appends language-specific module namespace aliases during namespace index construction.
    fn module_namespaces(
        &self,
        _profile: &dyn LanguageProfile,
        _node: &Node,
        _dependency_registry: &DependencyRegistry,
        _out: &mut Vec<String>,
    ) {
    }

    /// Resolves a builtin receiver property type when supported by the language's semantic context.
    fn resolve_builtin_property<'a>(
        &self,
        _path: &str,
        _receiver_name: &str,
        _field: &str,
        _semantic_context: &dyn BuiltinPropertyLookup,
    ) -> Option<TypeTarget<'a>> {
        None
    }

    /// Classifies builtin receiver origin tags for resolved builtin member references.
    fn builtin_member_origin(
        &self,
        _receiver: &TypeTarget<'_>,
        default_origin: Option<&'static str>,
        _computed_call: bool,
    ) -> Option<&'static str> {
        default_origin
    }

    /// Appends language-specific cross-symbol semantic relations for one file.
    fn compose_file_relations<'a>(&self, _ctx: &mut FileRelationContext<'_, 'a>) {}

    /// Resolves a parsed import against indexed modules and declarations.
    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        resolve_default_import(context)
    }

    /// Reports whether a receiver name is valid for its owning declaration.
    fn resolve_receiver(&self, profile: &dyn LanguageProfile, name: &str, owner: &Node) -> bool {
        profile.receiver(name, owner)
    }

    /// Resolves a member through language-specific companion declarations.
    #[allow(clippy::too_many_arguments)]
    fn resolve_imported_member<'a>(
        &self,
        _interner: &LinkerInterner<'a>,
        _module: &'a Node,
        _member: &str,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _by_qual: &HashMap<&'a str, Vec<&'a Node>>,
        _exports: &PackageExports<'a>,
        _lookup_key: &mut String,
    ) -> Vec<&'a Node> {
        Vec::new()
    }

    /// Builds language-specific package re-export indexes before parallel linking.
    fn package_exports<'a>(
        &self,
        _interner: &mut LinkerInterner<'a>,
        _node_rebindings: &NodeBindingIndex,
        _all: &'a dyn FactsLookup,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&Path>,
    ) -> PackageExports<'a> {
        HashMap::new()
    }

    /// Returns files whose full facts are required to relink affected owners.
    fn required_full_facts(
        &self,
        _facts: &dyn FactsLookup,
        _affected: &BTreeSet<String>,
        _catalog: &[(&str, &str)],
    ) -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// Reports whether a file's references can change identity when it changes.
    fn needs_reference_identity(&self, _path: &str, _facts: &Facts) -> bool {
        false
    }

    /// Compares language-specific reference identity across an incremental update.
    fn reference_identity_changed(&self, _path: &str, _old: &Facts, _new: &Facts) -> bool {
        false
    }

    /// Reports whether an indexed path is a declaration-only companion file.
    fn is_declaration_only(&self, _path: &str) -> bool {
        false
    }

    fn apply_wildcard_import<'a, 'file>(
        &self,
        _state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>,
        _reference: &'a Reference,
        _profile: Option<&dyn LanguageProfile>,
        _scope: &'file str,
        _modules: &[&'a Node],
    ) -> bool
    where
        'a: 'file,
    {
        false
    }
    fn apply_aliased_external_fallback<'a, 'file>(
        &self,
        _state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>,
        _reference: &'a Reference,
        _normalized: Option<&ImportPath>,
        _scope: &'file str,
        _alias: &'a str,
    ) where
        'a: 'file,
    {
    }
    fn apply_module_star_reexports<'a, 'file>(
        &self,
        _state: &mut crate::linker::traits::ImportScopeState<'a, '_, 'file>,
    ) where
        'a: 'file,
    {
    }
    fn import_edge_owner<'a>(
        &self,
        _owner: &'a Node,
        _by_id: &HashMap<SymbolId, &'a Node>,
        _interner: &LinkerInterner<'a>,
        _path: &str,
    ) -> Option<&'a str> {
        None
    }
    fn admits_imported_declaration_edge(&self, _declaration: &Node, _target_module: &Node) -> bool {
        false
    }
    fn emits_single_imported_target_edge(&self) -> bool {
        false
    }
    #[allow(clippy::too_many_arguments)]
    fn type_only_import_is_unproven(
        &self,
        _profile: &dyn LanguageProfile,
        _path: &str,
        _facts: &Facts,
        _modules: &ModulesByNamespace<'_>,
        _dependencies: &DependencyRegistry,
        _interner: &LinkerInterner<'_>,
        _node_rebindings: &NodeBindingIndex,
    ) -> bool {
        false
    }
    fn checks_annotation_binding_shadowing(&self) -> bool {
        true
    }
    fn module_shadows_annotation(&self) -> bool {
        false
    }
    fn is_dynamic_typing_annotation(
        &self,
        _head: &str,
        _member: &str,
        _owner: &Node,
        _aliases: &HashMap<&str, HashMap<&str, Vec<&Node>>>,
        _dynamic_aliases: &HashMap<&str, hashbrown::HashSet<&str>>,
    ) -> bool {
        false
    }
    fn admits_external_generic_annotation(
        &self,
        _profile: Option<&dyn LanguageProfile>,
        _raw_annotation: &str,
        _head: &str,
        _module: &str,
    ) -> bool {
        true
    }
    #[allow(clippy::too_many_arguments)]
    fn external_parameter_annotation<'a>(
        &self,
        _profile: Option<&dyn LanguageProfile>,
        _raw_annotation: &'a str,
        _raw_head: &'a str,
        _head: &str,
        _member: &str,
        _path: &str,
        _dependencies: &DependencyRegistry,
        _try_qualified: bool,
    ) -> Option<crate::linker::types::ReceiverType<'a>> {
        None
    }
    fn normalize_dom_receiver<'a>(&self, receiver: &'a str) -> &'a str {
        receiver
    }
    fn dom_property_type(&self, _receiver: &str, _member: &str) -> Option<&'static str> {
        None
    }
    fn symbol_visible(&self, _owner: &Node, _candidate: &Node) -> Option<bool> {
        None
    }
    fn admits_classified_node(&self, _node: &Node) -> bool {
        true
    }
    fn is_constructor_symbol(&self, _node: &Node) -> bool {
        false
    }
    fn admits_component_template_field(&self, _receiver: &Node) -> bool {
        false
    }
    fn is_dynamic_type_alias(&self, _module: &str, _expression: &str) -> bool {
        false
    }
    fn external_factory_import_is_safe(
        &self,
        _module: &Node,
        _alias: &str,
        _position: crate::semantic::SourcePosition,
    ) -> bool {
        false
    }
    fn prepare_semantic_context<'a>(
        &self,
        _context: &mut crate::linker::semantic_context::context::Context<'a>,
        _prep: &crate::linker::traits::SemanticContextPrep<'a>,
    ) {
    }
    fn collects_argument_factory_rules(&self, _language_id: &str) -> bool {
        false
    }
    fn resolve_special_type_expr<'a>(
        &self,
        _owner: &'a Node,
        _expression: &crate::semantic::TypeExpr,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
        _limits: crate::linker::value_flow::SemanticLimits,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn special_type_alias_target<'a>(
        &self,
        _node: &'a Node,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn stored_type_expr(&self, _annotation: &str) -> Option<crate::semantic::TypeExpr> {
        None
    }
    fn setup_scope_complete(&self, _owner: &Node, _parent_is_module: bool) -> bool {
        false
    }
    fn stable_immutable_capture(&self, _writes: &[crate::linker::value_flow::Binding<'_>]) -> bool {
        false
    }
    fn callable_target_return<'s, 'a>(
        &self,
        _index: &'s (dyn ValueFlowLookup<'a> + 'a),
        _target: &'s TypeTarget<'a>,
        _owner: &'a Node,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
        _trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&'s TypeTarget<'a>> {
        None
    }
    fn constructor_symbol<'a>(
        &self,
        _owner: &'a Node,
        _expression: &str,
        _at: crate::semantic::SourcePosition,
        _resolver: &dyn SemanticResolver<'a>,
    ) -> Option<crate::linker::value_flow::Symbol<'a>> {
        None
    }
    fn disambiguate_candidates(&self, _candidates: &mut Vec<&Node>) {}
    fn receiver_requires_static(&self) -> bool {
        false
    }
    fn is_local_unverified_request(
        &self,
        _node: &Node,
        _flow: &crate::semantic::ValueFlowFacts,
        _path: &str,
        _dependencies: &DependencyRegistry,
    ) -> bool {
        false
    }
    fn required_configuration_facts(
        &self,
        _facts: &std::collections::BTreeMap<String, crate::typed_facts::TypedFacts>,
        _catalog: &[(&str, &str)],
        _dependencies: &DependencyRegistry,
    ) -> BTreeSet<String> {
        BTreeSet::new()
    }
    fn collects_module_flow_rebindings(&self, _node: &Node) -> bool {
        false
    }
    fn collects_flow_bindings(&self, _node: &Node) -> bool {
        false
    }
    fn strips_component_macro_raw_details(&self, _node: &Node) -> bool {
        false
    }
    fn prefers_construct_contract(&self) -> bool {
        false
    }
    fn enforces_local_contract_position(&self) -> bool {
        false
    }
}

/// Generic linker used by profiles without language-specific resolution rules.
pub(crate) struct GenericLanguageLinker;

/// Shared generic linker instance.
pub(crate) static GENERIC_LINKER: GenericLanguageLinker = GenericLanguageLinker;

/// Crate-private dispatch for the cross-file linker associated with a profile.
pub(crate) trait ProfileLinker {
    fn linker(&self) -> &'static dyn LanguageLinker;
}

impl<T: LanguageProfile + ?Sized> ProfileLinker for T {
    fn linker(&self) -> &'static dyn LanguageLinker {
        match self.id() {
            #[cfg(feature = "lang-html")]
            "html" => &crate::languages::html::HTML_LINKER,
            #[cfg(feature = "lang-python")]
            "python" => &crate::linker::python::profile_linker::PYTHON_LINKER,
            #[cfg(feature = "lang-rust")]
            "rust" => &crate::linker::rust::linker_impl::RUST_LINKER,
            #[cfg(feature = "lang-typescript")]
            "typescript" | "javascript" | "vue" => {
                &crate::linker::typescript::profile_linker::TYPESCRIPT_LINKER
            }
            _ => &GENERIC_LINKER,
        }
    }
}

impl LanguageLinker for GenericLanguageLinker {
    fn allows_multi_file_modules(&self) -> bool {
        true
    }

    fn module_namespaces(
        &self,
        profile: &dyn LanguageProfile,
        node: &Node,
        dependency_registry: &DependencyRegistry,
        out: &mut Vec<String>,
    ) {
        for mp in dependency_registry.module_prefixes_for_path(profile.family(), &node.path) {
            if let Some((full, rel_mod)) = prefixed_module_namespace(profile, &node.path, mp) {
                for alias in [full, rel_mod] {
                    if !alias.is_empty() && !out.contains(&alias) {
                        out.push(alias);
                    }
                }
            }
        }
    }

    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        resolve_default_import_with_options(context, true)
    }
}

pub(crate) fn normalize_prefix_namespace(prefix: &str) -> String {
    prefix
        .trim_end_matches(['/', '\\', '.', ':'])
        .replace(['/', '\\', ':'], ".")
}

fn strip_prefix_segments<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return None;
    }
    if value == prefix {
        return Some("");
    }
    value
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
}

pub(crate) fn prefixed_module_namespace(
    profile: &dyn LanguageProfile,
    path: &str,
    mp: &ModulePrefix,
) -> Option<(String, String)> {
    let (_, local_path) = languages::workspace_path(path);
    let (_, root_local) = languages::workspace_path(&mp.source_root);
    let rel_path = if root_local.is_empty() {
        local_path
    } else {
        local_path
            .strip_prefix(root_local)
            .and_then(|rest| rest.strip_prefix('/'))?
    };
    let rel_mod = profile.module_name(rel_path);
    let norm_prefix = normalize_prefix_namespace(&mp.prefix);
    if norm_prefix.is_empty() {
        return None;
    }
    let full = if rel_mod.is_empty() {
        norm_prefix
    } else {
        format!("{norm_prefix}.{rel_mod}")
    };
    Some((full, rel_mod))
}

fn map_module_prefix(
    namespace: &str,
    prefixes: &[&ModulePrefix],
    symbol_path: bool,
) -> Option<ImportPath> {
    for mp in prefixes {
        let norm_prefix = normalize_prefix_namespace(&mp.prefix);
        if let Some(rest) = strip_prefix_segments(namespace, &norm_prefix) {
            let (_, root_local) = languages::workspace_path(&mp.source_root);
            let root_ns = if root_local.is_empty() {
                String::new()
            } else {
                normalize_prefix_namespace(root_local)
            };
            let mapped = match (root_ns.is_empty(), rest.is_empty()) {
                (true, true) => String::new(),
                (true, false) => rest.to_owned(),
                (false, true) => root_ns,
                (false, false) => format!("{root_ns}.{rest}"),
            };
            if !mapped.is_empty() {
                return Some(ImportPath {
                    namespace: mapped,
                    relative: false,
                    symbol_path,
                });
            }
        }
    }
    None
}

pub(crate) fn normalize_import_with_prefixes(
    profile: &dyn LanguageProfile,
    path: &str,
    module: &str,
    root: Option<&Path>,
    dependencies: &DependencyRegistry,
) -> Option<ImportPath> {
    let prefixes = dependencies.module_prefixes_for_path(profile.family(), path);
    if let Some(normalized) = profile.normalize_import_with_root(root, path, module) {
        if !normalized.relative && !prefixes.is_empty() {
            if let Some(mapped) =
                map_module_prefix(&normalized.namespace, &prefixes, normalized.symbol_path)
            {
                return Some(mapped);
            }
        }
        return Some(normalized);
    }
    (!prefixes.is_empty())
        .then(|| map_module_prefix(&normalize_prefix_namespace(module), &prefixes, true))
        .flatten()
}

pub(crate) fn resolve_default_import<'ctx, 'a, 'input>(
    context: &mut ImportContext<'ctx, 'a, 'input>,
) -> ImportResolution<'a> {
    resolve_default_import_with_options(context, false)
}

fn collect_module_candidates<'ctx, 'a, 'input>(
    context: &mut ImportContext<'ctx, 'a, 'input>,
    owner_prefix: Option<&str>,
    target_tail: &str,
    default_export_only: bool,
) {
    for module in context.modules.iter().copied() {
        context.lookup_key.clear();
        context.lookup_key.push_str(&module.qualname);
        if let Some(prefix_tail) = owner_prefix {
            if !prefix_tail.is_empty() {
                context.lookup_key.push('.');
                context.lookup_key.push_str(prefix_tail);
            }
            context.lookup_key.push('.');
        } else {
            context.lookup_key.push('.');
            context.lookup_key.push_str(target_tail);
        }
        let target = context.lookup_key.as_str();
        context.candidates.extend(
            context
                .by_module
                .get(module.path.as_str())
                .into_iter()
                .flatten()
                .copied()
                .filter(|node| {
                    if owner_prefix.is_some() {
                        node.kind != NodeKind::Component && node.qualname.starts_with(target)
                    } else if default_export_only {
                        *node.detail_entry(crate::models::details::NodeDetailKey::DEFAULT_EXPORT)
                            == true
                    } else {
                        node.kind != NodeKind::Component && node.qualname == target
                    }
                }),
        );
    }
}

pub(crate) fn resolve_default_import_with_options<'ctx, 'a, 'input>(
    context: &mut ImportContext<'ctx, 'a, 'input>,
    allow_multi_file: bool,
) -> ImportResolution<'a> {
    let Some(tail) = context.tail().map(str::to_owned) else {
        return ImportResolution::default();
    };
    if context.modules.is_empty() || (!allow_multi_file && context.modules.len() != 1) {
        return ImportResolution::default();
    }
    if tail.is_empty() || tail == "*" {
        context.candidates.extend(context.modules.iter().copied());
        return ImportResolution::default();
    }
    if let Some(owner_tail) = tail.strip_suffix(".*") {
        collect_module_candidates(context, Some(owner_tail), "", false);
        return ImportResolution::default();
    }
    collect_module_candidates(context, None, &tail, tail == "default");
    ImportResolution::default()
}

/// Supplies syntax extraction and normalization rules for one language.
///
/// Profiles are shared as `Send + Sync` statics. Default hooks keep extraction
/// bounded to a source file; cross-file resolution belongs to [`LanguageLinker`].
pub trait LanguageProfile: Send + Sync {
    /// Returns the stable language identifier used in indexed nodes.
    fn id(&self) -> &'static str;
    /// Lists manifest filenames recognized by this profile.
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &[]
    }
    /// Extracts dependency names from a recognized manifest file.
    fn extract_manifest_dependencies(&self, _filename: &str, _content: &str) -> Vec<String> {
        Vec::new()
    }
    /// Reports whether a module belongs to this language's standard library.
    fn is_stdlib(&self, _module: &str) -> bool {
        false
    }
    /// Returns the namespace family used for cross-profile import matching.
    fn family(&self) -> LanguageFamily {
        LanguageFamily(self.id())
    }
    /// Returns source file extensions accepted by this profile.
    fn extensions(&self) -> &'static [&'static str];
    /// Whether this profile supports Tree-sitter parsing and AST search.
    fn has_grammar(&self) -> bool {
        true
    }
    /// Selects the Tree-sitter grammar for a source path.
    fn grammar(&self, path: &str) -> tree_sitter::Language;
    /// Creates a Tree-sitter parser configured for the source path.
    ///
    /// # Errors
    ///
    /// Returns an error when Tree-sitter rejects the grammar.
    fn create_parser(&self, path: &str) -> Result<Parser> {
        anyhow::ensure!(
            self.has_grammar(),
            "AST search is not supported for {}",
            self.id()
        );
        let mut parser = Parser::new();
        parser.set_language(&self.grammar(path))?;
        Ok(parser)
    }
    /// Maps a grammar node kind to an indexed symbol category.
    fn symbol_kind(&self, kind: &str) -> Option<&'static str>;
    /// Supplies declarative syntax selectors; legacy profiles retain their hooks.
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        None
    }
    /// Creates incremental language body state consumed by the shared DFS.
    fn scope_analysis<'source, 'tree>(
        &self,
        _node: Syntax<'tree>,
        _source: &'source str,
        _file: &FileContext,
    ) -> Option<ScopeAnalysis<'source, 'tree>> {
        None
    }
    /// Classifies a syntax node as an indexed symbol when applicable.
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        self.symbol_kind(node.kind())
    }
    /// Classifies a syntax node using its source text when needed.
    fn symbol_with_source(&self, node: Syntax<'_>, _source: &str) -> Option<&'static str> {
        self.symbol(node)
    }
    /// Returns the identifier prefix for a symbol category.
    fn node_prefix(&self, kind: &str) -> &'static str;
    /// Derives a module name from a workspace-relative path.
    fn module_name(&self, path: &str) -> String {
        module_stem(path).replace('/', ".")
    }
    /// Derives a module name when source content can affect its namespace.
    fn module_name_for_source(&self, path: &str, _source: &str) -> String {
        self.module_name(path)
    }
    /// Reports whether a declaration in one file is visible to a sibling file.
    fn sibling_accessible(&self, _path_a: &str, _path_b: &str) -> bool {
        false
    }
    /// Normalizes an import expression against its owning source path.
    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath>;
    /// Normalizes an import when workspace root context is available.
    fn normalize_import_with_root(
        &self,
        _root: Option<&Path>,
        owner: &str,
        module: &str,
    ) -> Option<ImportPath> {
        self.normalize_import(owner, module)
    }
    /// Classifies declarations using the prepared effective rule context.
    fn symbol_with_context(
        &self,
        node: Syntax<'_>,
        source: &str,
        _file: &FileContext,
    ) -> Option<&'static str> {
        self.symbol_with_source(node, source)
    }
    /// Classifies declarations using precomputed node kind and DFS ancestors.
    fn symbol_with_ancestors(
        &self,
        node: Syntax<'_>,
        _node_kind: &str,
        _ancestors: &[Syntax<'_>],
        source: &str,
        file: &FileContext,
    ) -> Option<&'static str> {
        self.symbol_with_context(node, source, file)
    }
    /// Classifies a module as an external dependency when known.
    fn external_import(&self, _module: &str) -> Option<&'static str> {
        None
    }
    /// Reports whether a name is a language builtin.
    fn builtin(&self, _name: &str) -> bool {
        false
    }
    /// Reports whether a name is a builtin type.
    fn builtin_type(&self, _name: &str) -> bool {
        false
    }
    /// Reports whether a member is builtin on a receiver type.
    fn builtin_member(&self, _receiver: &str, _member: &str) -> bool {
        false
    }
    /// Reports whether a receiver names a builtin generic type.
    fn builtin_generic(&self, _receiver: &str) -> bool {
        false
    }
    /// Extracts documentation text attached to a syntax node.
    fn doc_comment(&self, node: Syntax<'_>, source: &str) -> String {
        doc_comment(node, source)
    }
    /// Extracts a symbol name borrowed from the source text.
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        symbol_name(node, source)
    }
    /// Initializes per-file extraction state before walking the syntax tree.
    fn prepare(&self, _root: Syntax<'_>, _source: &str) -> FileContext {
        FileContext::default()
    }
    /// Initializes grammar-owned state with the effective validated rule context.
    fn prepare_with_context(
        &self,
        root: Syntax<'_>,
        source: &str,
        rules: &Arc<LanguageContext>,
    ) -> FileContext {
        let mut file = self.prepare(root, source);
        file.rules = Arc::clone(rules);
        file
    }
    /// Derives module exports after the shared walk has collected root bindings.
    fn module_exports_with_bindings(
        &self,
        _root: Syntax<'_>,
        _source: &str,
        _bindings: &ScopeBindings,
    ) -> Option<Vec<crate::semantic::ExportBinding>> {
        None
    }
    /// Projects bounded semantic metadata for an indexed declaration.
    fn metadata(
        &self,
        _node: Syntax<'_>,
        _source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> SymbolMetadata {
        SymbolMetadata::default()
    }
    /// Projects declaration metadata using precomputed node kind and DFS ancestors.
    fn metadata_with_ancestors(
        &self,
        node: Syntax<'_>,
        _node_kind: &str,
        _ancestors: &[Syntax<'_>],
        source: &str,
        name: &str,
        file: &FileContext,
    ) -> SymbolMetadata {
        self.metadata(node, source, name, file)
    }
    /// Projects supported test declarations into stable semantic roles.
    fn test_role(
        &self,
        _node: Syntax<'_>,
        _source: &str,
        _path: &str,
        _file: &FileContext,
    ) -> Option<&'static str> {
        None
    }
    /// Returns a proven source-local entrypoint category for this declaration.
    fn entrypoint_role(
        &self,
        _node: Syntax<'_>,
        _source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> Option<&'static str> {
        None
    }
    /// Applies canonical module export facts to declaration visibility metadata.
    fn finalize_file_visibility(
        &self,
        _root: Syntax<'_>,
        _source: &str,
        _facts: &mut Facts,
        _exports: &[crate::semantic::ExportBinding],
    ) {
    }
    /// Analyzes one declaration, allowing profiles to share local intermediate facts.
    fn declaration(
        &self,
        node: Syntax<'_>,
        source: &str,
        name: &str,
        file: &FileContext,
    ) -> (SymbolMetadata, crate::semantic::ValueFlowFacts) {
        (
            self.metadata(node, source, name, file),
            self.value_flow_with_context(node, source, file),
        )
    }
    /// Reports whether a syntax node marks a test declaration.
    fn test_attribute(&self, _node: Syntax<'_>, _source: &str) -> bool {
        false
    }
    /// Reports whether this grammar needs class-scoped binding extraction.
    fn class_scope(&self) -> bool {
        false
    }
    /// Extracts local scope bindings from a syntax node.
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ScopeBindings {
        scope_bindings(node, source)
    }
    /// Extracts bounded local value-flow facts from a syntax node.
    fn value_flow(&self, _node: Syntax<'_>, _source: &str) -> crate::semantic::ValueFlowFacts {
        crate::semantic::ValueFlowFacts::default()
    }
    /// Extracts local value-flow facts using information prepared for the file.
    fn value_flow_with_context(
        &self,
        node: Syntax<'_>,
        source: &str,
        _file: &FileContext,
    ) -> crate::semantic::ValueFlowFacts {
        self.value_flow(node, source)
    }
    /// Reports whether a reference can use local value-flow bindings.
    fn value_binding_applies(&self, _reference: &Reference) -> bool {
        true
    }
    /// Reports whether a name denotes the receiver of its owning method.
    fn receiver(&self, name: &str, _owner: &Node) -> bool {
        name == "self" || name == "this" || name == "cls"
    }
    /// Appends import references found at the current syntax node.
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if self.syntax_grammar_spec().is_some() {
            let expression = field(ctx.node, ctx.source, "module_name")
                .or_else(|| field(ctx.node, ctx.source, "source"))
                .unwrap_or_else(|| text(ctx.node, ctx.source));
            ctx.import(facts, expression.to_owned(), None, None);
        }
    }
    /// Appends structural relations found at the current syntax node.
    fn extract_relations(&self, _ctx: &SyntaxContext<'_, '_>, _facts: &mut Facts) {}
    /// Appends call references found at the current syntax node.
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if self.syntax_grammar_spec().is_some() {
            let expression = field(ctx.node, ctx.source, "function")
                .unwrap_or_else(|| text(ctx.node, ctx.source));
            facts.references.push(Reference {
                source: ctx.owner.into(),
                expression: expression.into(),
                kind: "calls".into(),
                line: ctx.line(),
                column: ctx.node.start_position().column,
                alias: None,
                module: None,
                dynamic: false,
                receiver_hint: None,
            });
        }
    }
    /// Appends mutation references found at the current syntax node.
    fn extract_mutations(&self, _ctx: &SyntaxContext<'_, '_>, _facts: &mut Facts) {}
    /// Reports whether this profile extracts route facts and needs per-file route context.
    fn route_extraction_enabled(&self) -> bool {
        true
    }
    /// Appends route relationships found at the current syntax node.
    fn extract_routes(
        &self,
        _ctx: &SyntaxContext<'_, '_>,
        _facts: &mut Facts,
        _symbols: &StdHashMap<usize, String>,
    ) {
    }
    /// Extracts manifest-backed routes using rule and prefix context prepared once per file.
    fn extract_routes_with_context(
        &self,
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        symbols: &StdHashMap<usize, String>,
        rules: &LanguageContext,
        routes: &RouteContext,
    ) {
        if self.typed_scope_projections() {
            self.extract_routes_with_rule_ir(ctx, facts, symbols, rules, &rules.rule_ir, routes);
        } else {
            self.extract_routes(ctx, facts, symbols);
        }
    }
    /// Appends routes using a grammar-bound manifest dispatch table.
    #[doc(hidden)]
    fn extract_routes_with_rule_ir(
        &self,
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        symbols: &StdHashMap<usize, String>,
        rules: &LanguageContext,
        rule_ir: &RuleIRHandle,
        routes: &RouteContext,
    ) {
        if self.typed_scope_projections() {
            let mut projections = StdHashMap::new();
            self.extract_routes_with_typed_projections(
                ctx,
                facts,
                symbols,
                rules,
                rule_ir,
                routes,
                &mut projections,
            );
            publish_scope_projections(facts, projections);
        } else {
            self.extract_routes_with_context(ctx, facts, symbols, rules, routes);
        }
    }
    #[doc(hidden)]
    fn typed_scope_projections(&self) -> bool {
        false
    }
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    fn extract_routes_with_typed_projections(
        &self,
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        symbols: &StdHashMap<usize, String>,
        rules: &LanguageContext,
        rule_ir: &RuleIRHandle,
        routes: &RouteContext,
        _projections: &mut StdHashMap<String, crate::semantic::ValueFlowFacts>,
    ) {
        self.extract_routes_with_rule_ir(ctx, facts, symbols, rules, rule_ir, routes);
    }
    /// Finalizes extracted facts after the syntax walk.
    fn finish(&self, _facts: &mut Facts) {}
    /// Describes AST search syntax supported by this profile.
    fn ast_search_capabilities(&self) -> AstSearchCapabilities {
        AstSearchCapabilities::default()
    }
    /// Reports whether a syntax kind only wraps a searchable pattern.
    fn pattern_wrapper(&self, kind: &str) -> bool {
        matches!(
            kind,
            "module" | "program" | "source_file" | "expression_statement"
        )
    }
    /// Extracts symbols and references from a source file into bounded facts.
    ///
    /// # Errors
    ///
    /// Returns an error if the grammar cannot parse or extract the file.
    fn extract_file(
        &self,
        _path: &str,
        _source: &str,
        _module: &str,
        _facts: &mut Facts,
    ) -> Result<()> {
        anyhow::bail!(
            "grammar-backed profile '{}' must be extracted via ast::parse_file",
            self.id()
        )
    }
}

/// Request parameters passed to tree extraction callbacks from embedded profiles.
pub(crate) struct TreeExtractArgs<'a, 'tree> {
    pub(crate) profile: &'a dyn LanguageProfile,
    pub(crate) root: Syntax<'tree>,
    pub(crate) path: &'a str,
    pub(crate) source: &'a str,
    pub(crate) module: &'a str,
    pub(crate) owner: &'a str,
    pub(crate) language_context: &'a Arc<LanguageContext>,
}

/// Function pointer invoked by embedded-language profiles to extract a parsed syntax island.
pub(crate) type TreeExtractor = fn(
    TreeExtractArgs<'_, '_>,
    &mut Facts,
    Option<&mut crate::typed_facts::FlowStore>,
) -> ScopeBindings;

impl DependencyRegistry {
    /// Classifies an external module without workspace-origin context.
    pub fn classification(
        &self,
        profile: &dyn LanguageProfile,
        module: &str,
    ) -> Option<&'static str> {
        if module.is_empty() {
            return None;
        }
        if profile.is_stdlib(module) {
            return Some("standard library");
        }
        let declared = self
            .packages
            .get(&profile.family())
            .is_some_and(|packages| {
                if packages.contains(module) {
                    return true;
                }
                module.char_indices().any(|(index, character)| {
                    matches!(character, '.' | '/' | ':') && packages.contains(&module[..index])
                })
            });
        Some(if declared {
            "external dependency (manifest)"
        } else {
            "external dependency"
        })
    }

    /// Classifies an import using the nearest declaring manifest in its workspace.
    pub fn classification_for_path(
        &self,
        profile: &dyn LanguageProfile,
        module: &str,
        path: &str,
    ) -> Option<String> {
        let classification = self.classification(profile, module)?;
        if classification != "external dependency (manifest)" {
            return Some(classification.to_owned());
        }
        let workspace = languages::workspace_path(path).0;
        let mut directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        loop {
            if let Some(origins) = self
                .scoped_origins
                .get(&profile.family())
                .and_then(|scopes| scopes.get(directory))
            {
                let origin = origins.get(module).or_else(|| {
                    module.char_indices().rev().find_map(|(index, character)| {
                        matches!(character, '.' | '/' | ':')
                            .then(|| origins.get(&module[..index]))
                            .flatten()
                    })
                });
                if let Some(origin) = origin {
                    return Some(format!("external dependency (manifest: {origin})"));
                }
            }
            if directory.is_empty() || (!workspace.is_empty() && directory == workspace) {
                return Some("external dependency".to_owned());
            }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
}
