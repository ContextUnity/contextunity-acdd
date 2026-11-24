//! Core traits implemented by compiled language profiles and their linkers.

use crate::ast::{
    scope_analysis::ScopeAnalysis,
    syntax::{doc_comment, field, scope_bindings, symbol_name, text, ScopeBindings},
};
use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::{
    context::{publish_scope_projections, FileContext, LanguageContext, RuleIRHandle},
    dependency_registry::{parser::ModulePrefix, DependencyRegistry},
    manifests::FrameworkManifest,
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
#[allow(private_interfaces)]
pub struct FileRelationContext<'ctx, 'a> {
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
    fn has_display(&self, _receiver: &Node) -> bool {
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

pub trait LanguageLinker: Send + Sync {
    /// Resolves an explicitly rooted module reference.
    fn explicit_module_target(&self, _profile: &dyn LanguageProfile, _phase: &crate::linker::traits::ReferencePhase<'_, '_>) -> Option<crate::linker::traits::ExplicitModuleTarget> {
        None
    }

    /// Resolves language-specific property references before generic admission.
    fn property_reference(&self, _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>) -> bool {
        false
    }

    /// Resolves language-specific references after generic admission.
    fn special_reference(&self, _phase: &mut crate::linker::traits::ReferencePhase<'_, '_>) -> bool {
        false
    }

    /// Whether an expression starts with an explicitly rooted module prefix.
    fn explicit_module_prefix(&self, _expression: &str) -> bool {
        false
    }

    /// Resolves template builtin evidence when this profile owns template syntax.
    fn template_builtin(&self, _expression: &str, _rules: &[&crate::languages::manifests::syntax::SyntaxRule]) -> Option<String> {
        None
    }

    /// Resolves a platform builtin origin after configured rules are considered.
    fn platform_builtin_origin<'a>(&self, _profile: &dyn LanguageProfile, _first: &str, _configured: Option<&'a str>) -> Option<&'a str> {
        None
    }

    /// Classifies a language builtin and its reference evidence.
    fn builtin_evidence(&self, _first: &str, _expression: &str) -> Option<(&'static str, String)> {
        None
    }

    /// Reports an external annotation admitted by the language.
    fn external_annotation(&self, _profile: &dyn LanguageProfile, _head: &str, _path: &str, _dependencies: &DependencyRegistry) -> bool {
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

    /// Returns the unshadowed global type/constructor name guarded by an identity wrapper marker.
    fn identity_call_global(&self, _callee: &str) -> Option<&'static str> {
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

    /// Returns a declared framework's user rules for the owning source path.
    ///
    /// Framework data stays behind the existing language-linker seam. The
    /// dependency registry enforces package declaration and workspace scope.
    fn framework_manifest<'a>(
        &self,
        dependencies: &'a DependencyRegistry,
        family: LanguageFamily,
        path: &str,
        framework: &str,
        package: &str,
    ) -> Option<&'a FrameworkManifest> {
        dependencies.framework_manifest_for_path(family, path, framework, package)
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

    /// Reports whether unresolved external imports provide call aliases.
    fn tracks_external_aliases(&self) -> bool {
        false
    }
}

/// Generic linker used by profiles without language-specific resolution rules.
pub(crate) struct GenericLanguageLinker;

/// Shared generic linker instance.
pub(crate) static GENERIC_LINKER: GenericLanguageLinker = GenericLanguageLinker;

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

    fn tracks_external_aliases(&self) -> bool {
        true
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
        for module in context.modules.iter().copied() {
            context.lookup_key.clear();
            context.lookup_key.push_str(&module.qualname);
            if !owner_tail.is_empty() {
                context.lookup_key.push('.');
                context.lookup_key.push_str(owner_tail);
            }
            context.lookup_key.push('.');
            let prefix = context.lookup_key.as_str();
            context.candidates.extend(
                context
                    .by_module
                    .get(module.path.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| {
                        node.kind != NodeKind::Component.as_str()
                            && node.qualname.starts_with(prefix)
                    }),
            );
        }
        return ImportResolution::default();
    }
    for module in context.modules.iter().copied() {
        context.lookup_key.clear();
        context.lookup_key.push_str(&module.qualname);
        context.lookup_key.push('.');
        context.lookup_key.push_str(&tail);
        let target = context.lookup_key.as_str();
        context.candidates.extend(
            context
                .by_module
                .get(module.path.as_str())
                .into_iter()
                .flatten()
                .copied()
                .filter(|node| {
                    if tail == "default" {
                        node.details["default_export"] == true
                    } else {
                        node.kind != NodeKind::Component.as_str() && node.qualname == target
                    }
                }),
        );
    }
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
    /// Returns the cross-file linker owning import and receiver resolution for this profile.
    fn linker(&self) -> &'static dyn LanguageLinker {
        &GENERIC_LINKER
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
