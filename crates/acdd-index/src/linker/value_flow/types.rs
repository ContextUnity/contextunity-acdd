//! Core value-flow data types, semantic resolver traits, and type resolution leaves.

use crate::interner::SymbolId;
use crate::{
    models::Node,
    semantic::{SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts},
};
use hashbrown::HashMap;
use std::{borrow::Cow, cell::RefCell, collections::BTreeMap, sync::Arc};

pub(super) type Definitions<'a> = HashMap<(&'a str, SourcePosition), Vec<&'a Node>>;
pub(super) type NamedNodes<'a> = HashMap<(&'a str, &'a str), Vec<&'a Node>>;
pub(super) type CollectionElements<'a> =
    HashMap<&'a str, HashMap<String, Vec<(SourcePosition, TypeTarget<'a>)>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
/// Prepared, scoped authority for a finite external call result.
pub struct ConfiguredCallResult {
    /// Persisted configuration and rule provenance.
    pub evidence: String,
    /// Exact configured callee.
    pub callee: SymbolId,
    /// Finite callable members.
    pub members: Vec<SymbolId>,
}

/// Opaque nominal authority payload; manifest implementation types stay internal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredNominalReceiver {
    /// Validated effective nominal rule, visible only to the canonical shared consumer.
    pub(crate) rule: crate::languages::manifests::syntax::ReceiverTypeRule,
}

/// Effective finite receiver authority, retaining the originating manifest row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfiguredReceiverAuthority {
    /// Python hierarchy row, selected after lexical and C3 proof.
    Hierarchy {
        /// Originating effective framework pack.
        pack: String,
        /// Exact surviving ordered-base hierarchy row.
        rule: Arc<crate::languages::manifests::ClassHierarchyReceiverRule>,
    },
    /// Exact nominal receiver operation.
    Nominal {
        /// Originating effective framework pack.
        pack: String,
        /// Exact surviving syntax operation identity.
        rule_id: String,
        /// Opaque prepared nominal rule payload.
        receiver: Arc<ConfiguredNominalReceiver>,
    },
}

pub(crate) const MAX_CANDIDATE_BRANCH_JOINS: usize = 8;

pub(super) fn normalize_dotted_expression(expression: &str) -> Cow<'_, str> {
    let expression = expression.trim();
    if !expression.contains('.') {
        return Cow::Borrowed(expression);
    }
    if expression.split('.').all(|part| part.trim() == part) {
        return Cow::Borrowed(expression);
    }
    let mut normalized = String::with_capacity(expression.len());
    for (index, part) in expression.split('.').enumerate() {
        if index > 0 {
            normalized.push('.');
        }
        normalized.push_str(part.trim());
    }
    Cow::Owned(normalized)
}

pub(super) struct ComputedKey<'a> {
    pub(super) source: &'a str,
    pub(super) position: SourcePosition,
    pub(super) expression: &'a str,
    pub(super) kind: &'a str,
}
impl std::hash::Hash for ComputedKey<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(
            &(self.source, self.position, self.expression, self.kind),
            state,
        );
    }
}
impl hashbrown::Equivalent<(&str, SourcePosition, &str, &str)> for ComputedKey<'_> {
    fn equivalent(&self, key: &(&str, SourcePosition, &str, &str)) -> bool {
        self.source == key.0
            && self.position == key.1
            && self.expression == key.2
            && self.kind == key.3
    }
}

#[derive(Clone, Debug)]
/// Resolved receiver, callable, or nominal type target tracked during value-flow analysis.
pub enum TypeTarget<'a> {
    /// A lexically proven Rust closure.
    RustClosure,
    /// Finite rule result; missing members never acquire unrestricted external authority.
    ConfiguredReceiver {
        /// Effective row identity.
        authority: Arc<ConfiguredReceiverAuthority>,
        /// Original proven receiver/import provenance.
        origin: Box<TypeTarget<'a>>,
        /// Independently established platform fallback only.
        fallback: Option<Box<TypeTarget<'a>>>,
        /// Effective configuration identity.
        evidence: String,
    },
    /// Declaration node belonging to the indexed workspace.
    Local(&'a Node),
    /// Callable declaration node referenced directly as a first-class value.
    Callable(&'a Node),
    /// Type supplied by a dependency outside the indexed workspace.
    External {
        /// Import module that supplies the type.
        module: String,
        /// Source line of the import declaration.
        import_line: usize,
    },
    /// Keep the Rust wrapper identity and bounded pointee result for member lookup.
    RustDeref {
        /// Name of the verified standard wrapper type.
        wrapper: &'static str,
        /// Type of the value reached through Rust's Deref contract.
        target: Box<TypeTarget<'a>>,
    },
    /// Standard Rust Option or Result retaining its generic arguments.
    RustWrapper {
        /// Standard wrapper constructor.
        wrapper: RustWrapperKind,
        /// Successful payload type.
        ok: Box<TypeTarget<'a>>,
        /// Error payload for Result; absent for Option.
        err: Option<Box<TypeTarget<'a>>>,
    },
    /// Rust reference retaining mutability for adapter and iterator resolution.
    RustReference {
        /// Whether this reference is mutable.
        mutable: bool,
        /// Referenced type.
        target: Box<TypeTarget<'a>>,
    },
    /// A proven standard iterator with its yielded item type.
    RustIterator {
        /// Iterator item type.
        item: Box<TypeTarget<'a>>,
    },
    /// Standard Rust vector retaining its element type.
    RustCollection {
        /// Element type.
        item: Box<TypeTarget<'a>>,
    },
    /// Typed Rust container (`VecDeque`, `HashSet`, `BTreeSet`) retaining its base & element type.
    RustTypedContainer {
        /// Canonical standard container base name.
        base: &'static str,
        /// Element type.
        item: Box<TypeTarget<'a>>,
    },
    /// Rust slice retaining its element type.
    RustSlice {
        /// Element type.
        item: Box<TypeTarget<'a>>,
    },
    /// Rust tuple retaining the independently resolved type of each field.
    RustTuple(Vec<TypeTarget<'a>>),
    /// Language built-in primitive, stdlib, or synthetic runtime type name.
    Builtin(String),
    /// Inline object/map literal mapping member names to declaration nodes.
    Object(Arc<HashMap<String, &'a Node>>),
    /// A finite external result object, never a local declaration.
    ResultObject(Arc<ConfiguredCallResult>),
    /// A callable projected from a configured result, retaining its establishment.
    ExternalCallable {
        /// Immutable configuration authority.
        result: Arc<ConfiguredCallResult>,
        /// Lexical scope where the projection was established.
        owner: String,
        /// Completed lexical establishment position.
        position: SourcePosition,
        /// Exact public member.
        member: String,
    },
    /// Finite local alternatives from a complete or partially unknown branch join.
    Candidates {
        /// Stable, declaration-identity-deduplicated local type declarations.
        targets: Vec<&'a Node>,
        /// Whether every local control-flow alternative was proven.
        complete: bool,
        /// Whether more than the supported candidate bound was observed.
        exhausted: bool,
    },
    /// Return type carried by an explicitly typed callable (`Callable[[...], R]`).
    CallableReturn(Box<TypeTarget<'a>>),
    /// Class object type (`type[T]` / `typing.Type[T]`) instantiating `T`.
    ClassType(Box<TypeTarget<'a>>),
    /// Async Promise / Future wrapping an inner resolved type.
    Promise(Box<TypeTarget<'a>>),
    /// Language-agnostic tuple retaining the independently resolved type of each element.
    Tuple(Vec<TypeTarget<'a>>),
    /// Typed language-agnostic collection/queryset retaining its base receiver name & item type.
    Collection {
        /// Base receiver name (e.g. `django.db.models.QuerySet`, `NodeList`).
        base: String,
        /// Element/item type.
        item: Box<TypeTarget<'a>>,
    },
    /// Multiple mutually conflicting declarations or types.
    Ambiguous,
    /// Unresolved or dynamic value with no statically proven target.
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Standard Rust result-wrapper constructors with dedicated unwrapping propagation.
pub enum RustWrapperKind {
    /// The `Option<T>` wrapper.
    Option,
    /// The `Result<T, E>` wrapper.
    Result,
}

#[derive(Clone, Copy, Debug)]
/// Role distinguishing how a referenced symbol identifier is used.
pub enum SymbolRole {
    /// Static type annotation or nominal type position.
    Type,
    /// Direct instantiation / class construction position.
    Constructor,
    /// Callable or method invocation position.
    Callable,
}

/// Outcome of resolving a symbol reference in a value-flow scope.
pub enum Symbol<'a> {
    /// Resolved statically to a value or nominal type target.
    Type(TypeTarget<'a>),
    /// Resolved to a local callable declaration.
    Callable(&'a Node),
    /// Multiple distinct declarations matched with equal specificity.
    Ambiguous,
    /// No declaration or external type matched.
    Unknown,
}

/// Defines semantic resolver behavior.
pub trait SemanticResolver<'a> {
    /// Whether a language global remains unshadowed at this source position.
    fn unshadowed_global(&self, _scope: &'a Node, _name: &str, _at: SourcePosition) -> bool { false }
    /// Returns the statically proven nominal target of a Rust dereference implementation.
    fn deref_target(&self, _receiver: &'a Node) -> Option<&'a Node> {
        None
    }
    /// Result policy of an already-proven external member.
    fn resolve_member_result(
        &self,
        _receiver: &TypeTarget<'a>,
        _member: &str,
        _scope: &'a Node,
        _at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    /// Finite literal-factory selection after generic result proof.
    fn resolve_call_result(
        &self,
        _scope: &'a Node,
        _callee: &str,
        _site: Option<&crate::semantic::CallSiteEvidence>,
        _at: SourcePosition,
        _fallback: Option<TypeTarget<'a>>,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    /// Resolves a configured callable result through its canonical binding provider.
    fn resolve_factory_result(
        &self,
        _scope: &'a Node,
        _callee: &str,
        _at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    /// Checks a configured result member using the resolver's canonical symbol IDs.
    fn configured_result_member(&self, _result: &ConfiguredCallResult, _member: &str) -> bool {
        false
    }
    /// Resolves a bounded property projection from effective receiver data or platform semantics.
    fn property_type(&self, _scope: &'a Node, _receiver: &str, _member: &str) -> Option<String> {
        None
    }
    fn value_flow(&self, _node: &'a Node) -> Option<&'a ValueFlowFacts> {
        None
    }
    fn invalid_value_flow(&self, _node: &'a Node) -> bool {
        false
    }
    fn raw_value_flow(&self, _node: &'a Node) -> Option<&'a serde_json::Value> {
        None
    }
    fn has_typed_flows(&self) -> bool {
        false
    }
    fn resolve_symbol(
        &self,
        scope: &'a Node,
        name: &str,
        at: SourcePosition,
        role: SymbolRole,
    ) -> Symbol<'a>;
    fn resolve_external_factory(
        &self,
        _scope: &'a Node,
        _callee: &str,
        _at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        None
    }
    fn resolve_member(
        &self,
        _receiver: &TypeTarget<'a>,
        _member: &str,
        _scope: &'a Node,
        _at: SourcePosition,
    ) -> Symbol<'a> {
        Symbol::Unknown
    }
    /// Resolves an associated member (`Type::member`) on `receiver`, defaulting to `resolve_member`.
    fn resolve_associated_member(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        scope: &'a Node,
        at: SourcePosition,
    ) -> Symbol<'a> {
        self.resolve_member(receiver, member, scope, at)
    }
    fn resolve_applied(
        &self,
        _scope: &'a Node,
        _base: &str,
        _args: &[TypeExpr],
        _at: SourcePosition,
    ) -> TypeTarget<'a> {
        TypeTarget::Unknown
    }
    /// Resolves a payload only for an explicitly projected standard wrapper.
    fn resolve_payload(
        &self,
        _scope: &'a Node,
        _expression: &TypeExpr,
        _at: SourcePosition,
    ) -> TypeTarget<'a> {
        TypeTarget::Unknown
    }
    fn resolve_import_module(
        &self,
        _scope: &'a Node,
        _name: &str,
        _at: SourcePosition,
    ) -> Option<&'a Node> {
        None
    }
    /// Resolves a Python `super().member` lookup for `owner`'s enclosing class.
    fn resolve_super_member(&self, _owner: &'a Node, _member: &str) -> Symbol<'a> {
        Symbol::Unknown
    }
    /// Returns the typed callable signature for `node`, when extracted.
    fn callable_signature(&self, _node: &'a Node) -> Option<&'a crate::semantic::CallableSignature> {
        None
    }
}

/// Read-only interface for language value-flow helpers without depending on evaluation orchestration.
// Public until Wave 2a narrows LanguageLinker to pub(crate).
pub trait ValueFlowLookup<'a> {
    fn lookup_tracked(
        &self,
        owner: &Node,
        name: &str,
        at: SourcePosition,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>>;

    fn return_type_tracked(
        &self,
        callable: &Node,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>>;

    fn async_return_type(&self, node: &Node) -> Option<&TypeTarget<'a>>;

    fn field_type_tracked(
        &self,
        class: &Node,
        name: &str,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>>;

    fn type_of_expression_tracked(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>>;
}

#[derive(Clone, Copy, Debug)]
/// Bounded resource limits enforced while decoding and evaluating value-flow scopes.
pub struct SemanticLimits {
    /// Maximum value-flow facts evaluated for a single scope before marking it oversized.
    pub facts_per_scope: usize,
    /// Maximum byte length allowed for resolved type or alias names.
    pub name_bytes: usize,
    /// Maximum parent/alias indirection depth traversed during lookup.
    pub alias_depth: usize,
}

impl Default for SemanticLimits {
    fn default() -> Self {
        Self {
            facts_per_scope: crate::semantic::DEFAULT_FACTS_PER_SCOPE,
            name_bytes: 4_096,
            alias_depth: 64,
        }
    }
}

pub(crate) struct Binding<'a> {
    pub(super) position: SourcePosition,
    pub(in crate::linker) target: TypeTarget<'a>,
    pub(super) valid_until: Option<SourcePosition>,
    pub(super) restores_previous: bool,
    pub(in crate::linker) immutable_initializer: bool,
}

pub(super) enum BindingList<'a> {
    One(Binding<'a>),
    Many(Vec<Binding<'a>>),
}

impl<'a> BindingList<'a> {
    pub(super) fn push(&mut self, binding: Binding<'a>) {
        match self {
            Self::Many(bindings) => bindings.push(binding),
            Self::One(_) => {
                let Self::One(first) = std::mem::replace(self, Self::Many(Vec::new())) else {
                    unreachable!("single binding was replaced");
                };
                *self = Self::Many(vec![first, binding]);
            }
        }
    }
}

impl<'a> std::ops::Deref for BindingList<'a> {
    type Target = [Binding<'a>];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::One(binding) => std::slice::from_ref(binding),
            Self::Many(bindings) => bindings,
        }
    }
}

impl std::ops::DerefMut for BindingList<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::One(binding) => std::slice::from_mut(binding),
            Self::Many(bindings) => bindings,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct BindingEvent<'facts, 'arena> {
    // Decoded fallback values are temporary; names stay in the original facts arena.
    pub(super) name: &'arena str,
    pub(super) position: SourcePosition,
    pub(super) value: &'facts ValueExpr,
    pub(super) conditional: bool,
    pub(super) immutable_initializer: bool,
}

pub(super) struct Scope<'a> {
    pub(super) parents: Vec<&'a str>,
    pub(super) bindings: HashMap<&'a str, BindingList<'a>>,
    pub(super) invalid: bool,
    pub(super) module: bool,
}

/// Per-file summary of inferred variable bindings, field types, and return targets.
pub struct ValueFlowIndex<'a> {
    pub(super) scopes: HashMap<&'a str, Scope<'a>>,
    pub(super) returns: HashMap<&'a str, TypeTarget<'a>>,
    pub(super) async_returns: HashMap<&'a str, TypeTarget<'a>>,
    pub(super) return_collection_elements: HashMap<&'a str, TypeTarget<'a>>,
    pub(super) async_return_collection_elements: HashMap<&'a str, TypeTarget<'a>>,
    pub(super) fields: HashMap<&'a str, HashMap<String, TypeTarget<'a>>>,
    pub(super) component_slots: HashMap<&'a str, HashMap<(String, String), TypeTarget<'a>>>,
    pub(super) local_types: HashMap<&'a str, TypeTarget<'a>>,
    pub(super) computed: HashMap<(&'a str, SourcePosition, &'a str, &'a str), TypeTarget<'a>>,
    pub(super) collection_elements: CollectionElements<'a>,
    pub(super) unknown: TypeTarget<'a>,
    pub(super) ambiguous: TypeTarget<'a>,
    pub(super) limits: SemanticLimits,
}

impl<'a> ValueFlowIndex<'a> {
    /// Returns the declaration bound to `name` when `target` is an inline object literal.
    pub fn object_member(target: &TypeTarget<'a>, name: &str) -> Option<&'a Node> {
        match target {
            TypeTarget::Object(members) => members.get(name).copied(),
            _ => None,
        }
    }

    pub(in crate::linker) fn constant_type(&self, node: &Node) -> Option<&TypeTarget<'a>> {
        (node.kind == crate::models::tags::NodeKind::Constant)
            .then(|| self.returns.get(node.id.as_str()))
            .flatten()
    }
}

pub(super) enum ScopeFacts<'a> {
    Borrowed(&'a ValueFlowFacts),
    Decoded(Box<ValueFlowFacts>),
}

impl std::ops::Deref for ScopeFacts<'_> {
    type Target = ValueFlowFacts;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(facts) => facts,
            Self::Decoded(facts) => facts,
        }
    }
}

pub(super) static EMPTY_SCOPE_FACTS: std::sync::LazyLock<ValueFlowFacts> =
    std::sync::LazyLock::new(ValueFlowFacts::default);

pub(super) type InitializedScope<'a> = (&'a Node, ScopeFacts<'a>, Scope<'a>, TypeTarget<'a>);

#[derive(Default)]
pub(crate) struct Epochs<'a> {
    pub(super) returns: HashMap<&'a str, u64>,
    pub(super) scopes: HashMap<&'a str, u64>,
    pub(super) fields: u64,
}

#[derive(Default)]
pub(crate) struct ReadEpochs<'a> {
    pub(super) returns: Vec<(&'a str, u64)>,
    pub(super) scopes: Vec<(&'a str, u64)>,
    pub(super) fields: Option<u64>,
}

impl ReadEpochs<'_> {
    pub(super) fn dirty(&self, epochs: &Epochs<'_>) -> bool {
        self.returns
            .iter()
            .any(|(id, epoch)| epochs.returns.get(id).copied().unwrap_or(0) != *epoch)
            || self
                .scopes
                .iter()
                .any(|(id, epoch)| epochs.scopes.get(id).copied().unwrap_or(0) != *epoch)
            || self.fields.is_some_and(|epoch| epoch != epochs.fields)
    }
}

/// Dependency epoch recorder captured while evaluating memoized value-flow queries.
// Public until Wave 2a narrows LanguageLinker to pub(crate).
pub struct ReadTrace<'a, 'b> {
    epochs: &'b Epochs<'a>,
    pub(super) reads: RefCell<ReadEpochs<'a>>,
}

impl<'a, 'b> ReadTrace<'a, 'b> {
    pub(super) fn new(epochs: &'b Epochs<'a>, mut reads: ReadEpochs<'a>) -> Self {
        reads.returns.clear();
        reads.scopes.clear();
        reads.fields = None;
        Self {
            epochs,
            reads: RefCell::new(reads),
        }
    }

    pub(in crate::linker) fn returned(&self, id: &'a str) {
        let mut reads = self.reads.borrow_mut();
        if !reads.returns.iter().any(|(read, _)| *read == id) {
            reads
                .returns
                .push((id, self.epochs.returns.get(id).copied().unwrap_or(0)));
        }
    }

    pub(super) fn scope(&self, id: &'a str) {
        let mut reads = self.reads.borrow_mut();
        if !reads.scopes.iter().any(|(read, _)| *read == id) {
            reads
                .scopes
                .push((id, self.epochs.scopes.get(id).copied().unwrap_or(0)));
        }
    }

    pub(super) fn fields(&self) {
        self.reads.borrow_mut().fields = Some(self.epochs.fields);
    }
}

pub(in crate::linker) fn node_position(node: &Node) -> SourcePosition {
    SourcePosition {
        line: node.line,
        column: node.detail_entry(crate::models::details::NodeDetailKey::COLUMN).as_u64().unwrap_or(0) as usize,
    }
}

pub(in crate::linker) fn import_path(module: &str) -> Cow<'_, str> {
    if module.contains("::") {
        Cow::Owned(module.replace("::", "."))
    } else {
        Cow::Borrowed(module)
    }
}

pub(super) fn reaching<'a, 'b>(
    writes: &'b [Binding<'a>],
    at: SourcePosition,
) -> Option<&'b TypeTarget<'a>> {
    if let [binding] = writes {
        return (binding.position < at && binding.valid_until.is_none_or(|until| at < until))
            .then_some(&binding.target);
    }
    let end = writes.partition_point(|binding| binding.position < at);
    for binding in writes[..end].iter().rev().take(32) {
        if binding.valid_until.is_none_or(|until| at < until) {
            return Some(&binding.target);
        }
        if !binding.restores_previous {
            return None;
        }
    }
    None
}

pub(super) fn duplicate_position(writes: &[Binding<'_>], index: usize) -> bool {
    let position = writes[index].position;
    index
        .checked_sub(1)
        .is_some_and(|previous| writes[previous].position == position)
        || writes
            .get(index + 1)
            .is_some_and(|next| next.position == position)
}

pub(in crate::linker) fn symbol_type(symbol: Symbol<'_>) -> TypeTarget<'_> {
    match symbol {
        Symbol::Type(target) => target,
        Symbol::Ambiguous => TypeTarget::Ambiguous,
        _ => TypeTarget::Unknown,
    }
}

pub(in crate::linker) fn collect_candidate_join<'a>(
    targets_iter: impl IntoIterator<Item = TypeTarget<'a>>,
) -> TypeTarget<'a> {
    let mut targets = BTreeMap::<String, &'a Node>::new();
    let mut complete = true;
    let mut exhausted = false;
    for target in targets_iter {
        match target {
            TypeTarget::Local(node)
                if matches!(
                    node.kind.as_str(),
                    "class" | "struct" | "interface" | "enum"
                ) =>
            {
                targets.insert(node.id.clone(), node);
            }
            TypeTarget::Candidates {
                targets: nested,
                complete: nested_complete,
                exhausted: nested_exhausted,
            } => {
                complete &= nested_complete;
                exhausted |= nested_exhausted;
                for node in nested {
                    targets.insert(node.id.clone(), node);
                }
            }
            _ => complete = false,
        }
        if targets.len() > MAX_CANDIDATE_BRANCH_JOINS {
            exhausted = true;
            complete = false;
            targets.clear();
            break;
        }
    }
    if exhausted {
        TypeTarget::Candidates {
            targets: Vec::new(),
            complete: false,
            exhausted: true,
        }
    } else {
        TypeTarget::Candidates {
            targets: targets.into_values().collect(),
            complete,
            exhausted: false,
        }
    }
}

pub(super) fn merge<'a>(left: &TypeTarget<'a>, right: &TypeTarget<'a>) -> TypeTarget<'a> {
    if matches!(
        left,
        TypeTarget::ResultObject(_)
            | TypeTarget::ExternalCallable { .. }
            | TypeTarget::ConfiguredReceiver { .. }
            | TypeTarget::RustDeref { .. }
            | TypeTarget::RustReference { .. }
            | TypeTarget::RustWrapper { .. }
            | TypeTarget::RustIterator { .. }
            | TypeTarget::RustCollection { .. }
            | TypeTarget::RustTypedContainer { .. }
            | TypeTarget::RustSlice { .. }
            | TypeTarget::CallableReturn(_)
            | TypeTarget::ClassType(_)
            | TypeTarget::Promise(_)
            | TypeTarget::Tuple(_)
            | TypeTarget::Collection { .. }
    ) && same_target(left, right)
    {
        return left.clone();
    }
    match (left, right) {
        (TypeTarget::Local(a), TypeTarget::Local(b)) if a.id == b.id => left.clone(),
        (TypeTarget::Callable(a), TypeTarget::Callable(b)) if a.id == b.id => left.clone(),
        (
            TypeTarget::External {
                module: a,
                import_line: x,
            },
            TypeTarget::External {
                module: b,
                import_line: y,
            },
        ) if a == b && x == y => left.clone(),
        (TypeTarget::Builtin(a), TypeTarget::Builtin(b)) if a == b => left.clone(),
        (
            TypeTarget::Candidates {
                targets: a,
                complete: ac,
                exhausted: ae,
            },
            TypeTarget::Candidates {
                targets: b,
                complete: bc,
                exhausted: be,
            },
        ) if ac == bc
            && ae == be
            && a.len() == b.len()
            && a.iter().zip(b).all(|(left, right)| left.id == right.id) =>
        {
            left.clone()
        }
        (TypeTarget::Unknown, _) | (_, TypeTarget::Unknown) => TypeTarget::Unknown,
        _ => TypeTarget::Ambiguous,
    }
}

pub(super) fn same_fields(
    left: &HashMap<&str, HashMap<String, TypeTarget<'_>>>,
    right: &HashMap<&str, HashMap<String, TypeTarget<'_>>>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, fields)| {
            right.get(id).is_some_and(|other| {
                fields.len() == other.len()
                    && fields.iter().all(|(name, value)| {
                        other
                            .get(name)
                            .is_some_and(|other| same_target(value, other))
                    })
            })
        })
}

pub(in crate::linker) fn same_target(left: &TypeTarget<'_>, right: &TypeTarget<'_>) -> bool {
    match (left, right) {
        (
            TypeTarget::ConfiguredReceiver {
                evidence: a,
                authority: ar,
                origin: ao,
                fallback: af,
            },
            TypeTarget::ConfiguredReceiver {
                evidence: b,
                authority: br,
                origin: bo,
                fallback: bf,
            },
        ) => {
            a == b
                && ar == br
                && same_target(ao, bo)
                && match (af, bf) {
                    (None, None) => true,
                    (Some(a), Some(b)) => same_target(a, b),
                    _ => false,
                }
        }
        (TypeTarget::ResultObject(a), TypeTarget::ResultObject(b)) => a == b,
        (
            TypeTarget::ExternalCallable {
                result: a,
                owner: ao,
                position: ap,
                member: am,
            },
            TypeTarget::ExternalCallable {
                result: b,
                owner: bo,
                position: bp,
                member: bm,
            },
        ) => a == b && ao == bo && ap == bp && am == bm,
        (TypeTarget::Local(a), TypeTarget::Local(b))
        | (TypeTarget::Callable(a), TypeTarget::Callable(b)) => a.id == b.id,
        (
            TypeTarget::External {
                module: a,
                import_line: x,
            },
            TypeTarget::External {
                module: b,
                import_line: y,
            },
        ) => a == b && x == y,
        (TypeTarget::Builtin(a), TypeTarget::Builtin(b)) => a == b,
        (TypeTarget::RustTuple(a), TypeTarget::RustTuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_target(a, b))
        }
        (
            TypeTarget::RustDeref {
                wrapper: aw,
                target: at,
            },
            TypeTarget::RustDeref {
                wrapper: bw,
                target: bt,
            },
        ) => aw == bw && same_target(at, bt),
        (
            TypeTarget::RustReference {
                mutable: am,
                target: at,
            },
            TypeTarget::RustReference {
                mutable: bm,
                target: bt,
            },
        ) => am == bm && same_target(at, bt),
        (
            TypeTarget::RustWrapper {
                wrapper: aw,
                ok: ao,
                err: ae,
            },
            TypeTarget::RustWrapper {
                wrapper: bw,
                ok: bo,
                err: be,
            },
        ) => {
            aw == bw
                && same_target(ao, bo)
                && match (ae, be) {
                    (None, None) => true,
                    (Some(a), Some(b)) => same_target(a, b),
                    _ => false,
                }
        }
        (TypeTarget::RustIterator { item: a }, TypeTarget::RustIterator { item: b }) => {
            same_target(a, b)
        }
        (TypeTarget::CallableReturn(a), TypeTarget::CallableReturn(b))
        | (TypeTarget::ClassType(a), TypeTarget::ClassType(b)) => same_target(a, b),
        (TypeTarget::RustCollection { item: a }, TypeTarget::RustCollection { item: b })
        | (TypeTarget::RustSlice { item: a }, TypeTarget::RustSlice { item: b }) => {
            same_target(a, b)
        }
        (
            TypeTarget::RustTypedContainer { base: ab, item: ai },
            TypeTarget::RustTypedContainer { base: bb, item: bi },
        ) => ab == bb && same_target(ai, bi),
        (
            TypeTarget::Candidates {
                targets: a,
                complete: ac,
                exhausted: ae,
            },
            TypeTarget::Candidates {
                targets: b,
                complete: bc,
                exhausted: be,
            },
        ) => {
            ac == bc
                && ae == be
                && a.len() == b.len()
                && a.iter().zip(b).all(|(left, right)| left.id == right.id)
        }
        (TypeTarget::Object(a), TypeTarget::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(name, node)| b.get(name).is_some_and(|other| other.id == node.id))
        }
        (TypeTarget::Promise(a), TypeTarget::Promise(b)) => same_target(a, b),
        (TypeTarget::Tuple(a), TypeTarget::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_target(a, b))
        }
        (
            TypeTarget::Collection { base: ab, item: ai },
            TypeTarget::Collection { base: bb, item: bi },
        ) => ab == bb && same_target(ai, bi),
        (TypeTarget::RustClosure, TypeTarget::RustClosure)
        | (TypeTarget::Unknown, TypeTarget::Unknown)
        | (TypeTarget::Ambiguous, TypeTarget::Ambiguous) => true,
        _ => false,
    }
}

pub(super) fn flow_dependent(value: &ValueExpr) -> bool {
    if let ValueExpr::Scoped { value, .. } | ValueExpr::Await { value } = value {
        return flow_dependent(value);
    }
    if matches!(value, ValueExpr::BranchJoin { .. }) {
        return true;
    }
    matches!(
        value,
        ValueExpr::Call { .. }
            | ValueExpr::Alias { .. }
            | ValueExpr::Field { .. }
            | ValueExpr::Project { .. }
            | ValueExpr::RustField { .. }
            | ValueExpr::RustProject { .. }
            | ValueExpr::LoopElement { .. }
            | ValueExpr::CallbackParam { .. }
    )
}

fn valid_immutable_initializers(facts: &ValueFlowFacts) -> bool {
    if facts.immutable_initializers.is_empty() {
        return true;
    }
    if facts
        .immutable_initializers
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return false;
    }
    let mut next = 0;
    for binding in &facts.bindings {
        let Some(&position) = facts.immutable_initializers.get(next) else {
            break;
        };
        if binding.position > position {
            return false;
        }
        if binding.position == position {
            if binding.conditional || matches!(&binding.value, ValueExpr::Unknown) {
                return false;
            }
            next += 1;
        }
    }
    next == facts.immutable_initializers.len()
}

fn valid_facts(facts: &ValueFlowFacts, limits: SemanticLimits) -> bool {
    let text = |name: &str| !name.is_empty() && name.len() <= limits.name_bytes;
    valid_immutable_initializers(facts)
        && facts
            .bindings
            .len()
            .saturating_add(facts.branch_joins.len())
            .saturating_add(facts.fields.len())
            .saturating_add(facts.collection_elements().count())
            .saturating_add(facts.vue_slots().count())
            <= limits.facts_per_scope
        && facts.collection_elements().all(|fact| {
            text(&fact.name) && fact.position.line > 0 && valid_type(&fact.element_type, limits, 0)
        })
        && facts.vue_slots().all(|fact| {
            text(&fact.slot)
                && text(&fact.binding)
                && fact.position.line > 0
                && valid_type(&fact.type_expr, limits, 0)
        })
        && facts.bindings.iter().all(|fact| {
            text(&fact.name) && fact.position.line > 0 && valid_value(&fact.value, limits, 0)
        })
        && facts.branch_joins.len() <= limits.facts_per_scope
        && facts.branch_joins.iter().all(|fact| {
            text(&fact.name) && fact.position.line > 0 && valid_value(&fact.value, limits, 0)
        })
        && facts.fields.iter().all(|fact| {
            text(&fact.name) && fact.position.line > 0 && valid_value(&fact.value, limits, 0)
        })
        && facts
            .return_type
            .as_ref()
            .is_none_or(|ty| valid_type(ty, limits, 0))
        && facts
            .alias_type
            .as_ref()
            .is_none_or(|ty| valid_type(ty, limits, 0))
        && facts.return_value.as_ref().is_none_or(|returned| {
            valid_value(returned, limits, 0)
                && facts
                    .return_position
                    .is_some_and(|position| position.line > 0)
        })
}

pub(super) fn valid_value(expr: &ValueExpr, limits: SemanticLimits, depth: usize) -> bool {
    if depth >= 8 {
        return false;
    }
    let text = |name: &str| !name.is_empty() && name.len() <= limits.name_bytes;
    match expr {
        ValueExpr::Alias { name } => text(name),
        ValueExpr::Annotated { type_expr } => valid_type(type_expr, limits, 0),
        ValueExpr::Unknown | ValueExpr::RustClosure => true,
        ValueExpr::Construct { callee } => text(callee),
        ValueExpr::Call { callee, site } => {
            text(callee)
                && expr.valid_call_sites(depth)
                && site
                    .as_ref()
                    .and_then(|site| site.receiver.as_deref())
                    .is_none_or(|receiver| valid_value(receiver, limits, depth + 1))
        }
        ValueExpr::Await { value: inner } | ValueExpr::RustProject { value: inner, .. } => valid_value(inner, limits, depth + 1),
        ValueExpr::LoopElement { iterable, body_end } => text(iterable) && body_end.line > 0,
        ValueExpr::Scoped {
            value: inner,
            body_end,
        } => body_end.line > 0 && valid_value(inner, limits, depth + 1),
        ValueExpr::BranchJoin { alternatives } => {
            !alternatives.is_empty()
                && alternatives.len() <= limits.facts_per_scope
                && alternatives
                    .iter()
                    .all(|alternative| valid_value(alternative, limits, depth + 1))
        }
        ValueExpr::Field { receiver, member } => text(receiver) && text(member),
        ValueExpr::Project {
            value: inner,
            member,
        } | ValueExpr::RustField {
            value: inner,
            member,
        } => text(member) && valid_value(inner, limits, depth + 1),
        ValueExpr::CallbackParam { callee, .. } => valid_value(callee, limits, depth + 1),
        ValueExpr::Object { members, .. } => {
            members.len() <= limits.facts_per_scope
                && members.iter().all(|member| {
                    text(&member.name)
                        && text(member.declaration_name())
                        && member.position.line > 0
                })
        }
    }
}

pub(crate) fn valid_scope_facts(
    node: &Node,
    facts: &ValueFlowFacts,
    limits: SemanticLimits,
) -> bool {
    if !valid_facts(facts, limits) {
        return false;
    }
    if facts.vue_slots().next().is_some()
        && (node.kind != crate::models::tags::NodeKind::Module
            || !facts
                .component_macros()
                .any(|fact| fact.role == crate::semantic::ComponentMacroRole::Slots))
    {
        return false;
    }
    if matches!(
        node.kind.as_str(),
        "function" | "method" | "lambda" | "template_scope"
    ) && (node.binding_count() > limits.facts_per_scope
        || node
            .binding_names()
            .any(|name| name.len() > limits.name_bytes))
    {
        return false;
    }
    node.parameter_count()
        .saturating_add(crate::typed_facts::scope_fact_count(facts))
        <= limits.facts_per_scope
        && node
            .parameter_types()
            .all(|(name, _)| name.len() <= limits.name_bytes)
}

pub(super) fn valid_type(expression: &TypeExpr, limits: SemanticLimits, depth: usize) -> bool {
    if depth >= limits.alias_depth {
        return false;
    }
    let name = |name: &str| !name.is_empty() && name.len() <= limits.name_bytes;
    match expression {
        TypeExpr::Named { name: text } => name(text),
        TypeExpr::Applied { base, args } => {
            name(base)
                && args.len() <= limits.facts_per_scope
                && args.iter().all(|arg| valid_type(arg, limits, depth + 1))
        }
        TypeExpr::RustRange { item } | TypeExpr::RustArray { item } => valid_type(item, limits, depth + 1),
        TypeExpr::RustCallArguments { args } => args.len() <= limits.facts_per_scope && args.iter().all(|arg| valid_type(arg, limits, depth + 1)),
        TypeExpr::Unknown => true,
    }
}
