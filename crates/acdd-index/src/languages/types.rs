use crate::ast::syntax::{bounded_expression, field, text};
use crate::models::{Facts, Reference};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};
pub(crate) use tree_sitter::Node as Syntax;

pub(crate) const MAX_VALUE_FLOW_TYPE_DEPTH: usize = 8;

/// Supported language ecosystems for modular dependency manifest parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Ecosystem {
    Rust,
    JavaScript,
    Python,
    Go,
    Jvm,
    Php,
    CSharp,
    Ruby,
    CCpp,
}

impl Ecosystem {
    /// Resolves a canonical `LanguageFamily` name into its governing `Ecosystem`.
    pub(crate) fn from_family(family: &str) -> Option<Self> {
        match family {
            "rust" => Some(Self::Rust),
            "javascript" => Some(Self::JavaScript),
            "python" => Some(Self::Python),
            "go" => Some(Self::Go),
            "java" | "kotlin" => Some(Self::Jvm),
            "php" => Some(Self::Php),
            "csharp" => Some(Self::CSharp),
            "ruby" => Some(Self::Ruby),
            "c" | "cpp" => Some(Self::CCpp),
            _ => None,
        }
    }

    /// Returns all `LanguageFamily` identities governed by this ecosystem's manifests.
    pub(crate) const fn families(self) -> &'static [LanguageFamily] {
        match self {
            Self::Rust => &[LanguageFamily("rust")],
            Self::JavaScript => &[LanguageFamily("javascript")],
            Self::Python => &[LanguageFamily("python")],
            Self::Go => &[LanguageFamily("go")],
            Self::Jvm => &[LanguageFamily("java"), LanguageFamily("kotlin")],
            Self::Php => &[LanguageFamily("php")],
            Self::CSharp => &[LanguageFamily("csharp")],
            Self::Ruby => &[LanguageFamily("ruby")],
            Self::CCpp => &[LanguageFamily("c"), LanguageFamily("cpp")],
        }
    }
}

pub(crate) fn profile_available(id: &str) -> bool {
    let provider = if id == "javascript" {
        "typescript"
    } else {
        id
    };
    matches!(
        provider,
        "bash"
            | "c"
            | "cpp"
            | "csharp"
            | "css"
            | "go"
            | "html"
            | "java"
            | "json"
            | "kotlin"
            | "php"
            | "proto"
            | "python"
            | "ruby"
            | "rust"
            | "sql"
            | "toml"
            | "typescript"
            | "vue"
            | "yaml"
    )
}

pub(crate) fn family_for_language(id: &str) -> Option<LanguageFamily> {
    if !profile_available(id) {
        return None;
    }
    Some(match id {
        "html" | "vue" | "javascript" | "typescript" => LanguageFamily("javascript"),
        "cpp" => LanguageFamily("cpp"),
        "c" => LanguageFamily("c"),
        "bash" => LanguageFamily("bash"),
        "csharp" => LanguageFamily("csharp"),
        "css" => LanguageFamily("css"),
        "go" => LanguageFamily("go"),
        "java" => LanguageFamily("java"),
        "json" => LanguageFamily("json"),
        "kotlin" => LanguageFamily("kotlin"),
        "php" => LanguageFamily("php"),
        "proto" => LanguageFamily("proto"),
        "python" => LanguageFamily("python"),
        "ruby" => LanguageFamily("ruby"),
        "rust" => LanguageFamily("rust"),
        "sql" => LanguageFamily("sql"),
        "toml" => LanguageFamily("toml"),
        "yaml" => LanguageFamily("yaml"),
        _ => return None,
    })
}

pub(crate) fn compiled_language_families() -> impl Iterator<Item = &'static str> {
    let candidates: &[Option<&'static str>] = &[
        #[cfg(feature = "lang-bash")]
        Some("bash"),
        #[cfg(feature = "lang-c")]
        Some("c"),
        #[cfg(feature = "lang-cpp")]
        Some("cpp"),
        #[cfg(feature = "lang-csharp")]
        Some("csharp"),
        #[cfg(feature = "lang-css")]
        Some("css"),
        #[cfg(feature = "lang-go")]
        Some("go"),
        #[cfg(feature = "lang-java")]
        Some("java"),
        #[cfg(feature = "lang-json")]
        Some("json"),
        #[cfg(feature = "lang-kotlin")]
        Some("kotlin"),
        #[cfg(feature = "lang-php")]
        Some("php"),
        #[cfg(feature = "lang-proto")]
        Some("proto"),
        #[cfg(feature = "lang-python")]
        Some("python"),
        #[cfg(feature = "lang-ruby")]
        Some("ruby"),
        #[cfg(feature = "lang-rust")]
        Some("rust"),
        Some("sql"),
        #[cfg(feature = "lang-toml")]
        Some("toml"),
        #[cfg(any(feature = "lang-typescript", feature = "lang-html", feature = "lang-vue"))]
        Some("javascript"),
        #[cfg(feature = "lang-yaml")]
        Some("yaml"),
    ];
    candidates.iter().copied().flatten()
}

struct ThreadLocalParsers {
    builtin: [Vec<tree_sitter::Parser>; 6],
    other: HashMap<&'static str, Vec<tree_sitter::Parser>>,
}

impl ThreadLocalParsers {
    fn new() -> Self {
        Self {
            builtin: std::array::from_fn(|_| Vec::new()),
            other: HashMap::new(),
        }
    }

    fn builtin_index(key: &str) -> Option<usize> {
        Some(match key {
            "python" => 0,
            "rust" => 1,
            "html" => 2,
            "javascript" => 3,
            "typescript-tsx" => 4,
            "typescript" => 5,
            _ => return None,
        })
    }

    fn take(&mut self, key: &'static str) -> Option<tree_sitter::Parser> {
        if let Some(index) = Self::builtin_index(key) {
            self.builtin[index].pop()
        } else {
            self.other.get_mut(key).and_then(Vec::pop)
        }
    }

    fn give_back(&mut self, key: &'static str, parser: tree_sitter::Parser) {
        if let Some(index) = Self::builtin_index(key) {
            self.builtin[index].push(parser);
        } else {
            self.other.entry(key).or_default().push(parser);
        }
    }
}

thread_local! {
    static THREAD_LOCAL_PARSERS: RefCell<ThreadLocalParsers> =
        RefCell::new(ThreadLocalParsers::new());
}

/// Executes a closure with a warm, thread-local Tree-sitter parser configured for the grammar.
pub fn with_warm_parser<R>(
    lang_key: &'static str,
    grammar: tree_sitter::Language,
    f: impl FnOnce(&mut tree_sitter::Parser) -> R,
) -> anyhow::Result<R> {
    let parser = THREAD_LOCAL_PARSERS.with(|cell| -> anyhow::Result<tree_sitter::Parser> {
        let mut pool = cell.borrow_mut();
        if let Some(mut p) = pool.take(lang_key) {
            p.reset();
            p.set_language(&grammar)?;
            Ok(p)
        } else {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&grammar)?;
            Ok(p)
        }
    })?;

    struct ReturnGuard {
        lang_key: &'static str,
        parser: Option<tree_sitter::Parser>,
    }
    impl Drop for ReturnGuard {
        fn drop(&mut self) {
            if let Some(p) = self.parser.take() {
                THREAD_LOCAL_PARSERS.with(|cell| {
                    if let Ok(mut pool) = cell.try_borrow_mut() {
                        pool.give_back(self.lang_key, p);
                    }
                });
            }
        }
    }

    let mut guard = ReturnGuard {
        lang_key,
        parser: Some(parser),
    };

    let result = f(guard.parser.as_mut().expect("parser present"));
    Ok(result)
}

pub(crate) fn source_start(node: Syntax<'_>) -> crate::semantic::SourcePosition {
    let position = node.start_position();
    crate::semantic::SourcePosition {
        line: position.row + 1,
        column: position.column,
    }
}

pub(crate) fn source_end(node: Syntax<'_>) -> crate::semantic::SourcePosition {
    let position = node.end_position();
    crate::semantic::SourcePosition {
        line: position.row + 1,
        column: position.column,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
/// Import namespace shared by language profiles that can resolve one another.
pub struct LanguageFamily(pub &'static str);

/// Normalized module namespace and import interpretation for the linker.
pub struct ImportPath {
    /// Canonical namespace used for module lookup.
    pub namespace: String,
    /// Whether the import is relative to its owning module.
    pub relative: bool,
    /// Whether the namespace includes an imported symbol suffix.
    pub symbol_path: bool,
}
impl ImportPath {
    /// Creates an absolute module import with no symbol suffix.
    pub fn absolute(namespace: String) -> Self {
        Self {
            namespace,
            relative: false,
            symbol_path: false,
        }
    }
}

/// Typed per-file extraction state for Python profiles.
#[derive(Default)]
pub struct PythonFileState {
    /// Whether the file mentions `TYPE_CHECKING` and needs `if TYPE_CHECKING:` ancestor checks.
    pub has_type_checking: bool,
    /// Module-level `TYPE_CHECKING` alias imports and subsequent rebindings.
    pub type_checking_aliases: HashMap<String, Vec<(usize, bool)>>,
}

/// Typed per-file extraction state for TypeScript and JavaScript profiles.
#[derive(Default)]
pub struct TypeScriptFileState {
    /// Names exported as defaults.
    pub default_exports: HashSet<String>,
    /// Local bindings imported from `node:test` or configured test runners as executable test cases.
    pub test_case_imports: HashSet<String>,
    /// Local bindings imported from `node:test` as test fixtures.
    pub test_fixture_imports: HashSet<String>,
    /// Scope identifiers where a local binding shadows CommonJS `require`.
    pub shadowed_require_scopes: Arc<HashSet<usize>>,
    /// Scope identifiers where a source binding shadows the standard Array type.
    pub shadowed_array_type_scopes: Arc<HashSet<usize>>,
    /// Lexical scopes where a binding or assignment masks a DOM global.
    pub shadowed_dom_global_scopes: Arc<HashMap<usize, HashSet<String>>>,
    /// Exact DOM receiver parameter annotations indexed by function syntax node.
    pub dom_listener_receivers: Arc<HashMap<usize, HashMap<String, String>>>,
    /// Direct class field DOM annotations; duplicate or unknown fields have no type.
    pub dom_listener_class_fields: Arc<HashMap<usize, HashMap<String, Option<String>>>>,
    /// Source positions of lexical receiver writes within each enclosing scope.
    pub dom_listener_local_writes: Arc<HashMap<usize, HashMap<String, Vec<usize>>>>,
    /// Exact direct lexical declarations used to bound DOM receiver factory flow.
    pub dom_listener_local_declarations:
        Arc<HashMap<usize, HashMap<String, DomListenerLocalDeclaration>>>,
    /// Parameter and catch bindings that shadow DOM receiver names in their scope.
    pub dom_listener_parameter_bindings: Arc<HashMap<usize, HashSet<String>>>,
    /// Function scopes and names whose parameters are reassigned or rebound.
    pub rebound_function_parameters: Arc<HashMap<usize, HashSet<String>>>,
}

/// Typed per-file extraction state for Rust profiles.
#[derive(Default)]
pub struct RustFileState {
    /// File-declared function and method names available to forward references.
    pub declared_callables: HashSet<String>,
}

/// Typed language-owned extraction state stored on [`FileContext`].
#[derive(Default)]
pub enum LanguageFileState {
    /// Profile requires no language-specific per-file state.
    #[default]
    None,
    /// Python-specific per-file extraction state.
    Python(PythonFileState),
    /// Rust-specific per-file extraction state.
    Rust(RustFileState),
    /// TypeScript/JavaScript-specific per-file extraction state.
    TypeScript(TypeScriptFileState),
}

impl LanguageFileState {
    /// Borrows Python state when active.
    pub fn python(&self) -> Option<&PythonFileState> {
        match self {
            Self::Python(state) => Some(state),
            _ => None,
        }
    }

    /// Borrows Rust state when active.
    pub fn rust(&self) -> Option<&RustFileState> {
        match self {
            Self::Rust(state) => Some(state),
            _ => None,
        }
    }

    /// Borrows TypeScript/JavaScript state when active.
    pub fn typescript(&self) -> Option<&TypeScriptFileState> {
        match self {
            Self::TypeScript(state) => Some(state),
            _ => None,
        }
    }

    /// Ensures and mutably borrows TypeScript/JavaScript state.
    pub fn typescript_mut(&mut self) -> &mut TypeScriptFileState {
        if !matches!(self, Self::TypeScript(_)) {
            *self = Self::TypeScript(TypeScriptFileState::default());
        }
        match self {
            Self::TypeScript(state) => state,
            _ => unreachable!(),
        }
    }
}

/// Typed declaration role mapped onto [`crate::models::tags::NodeKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclarationRole {
    /// Top-level or nested function declaration.
    Function,
    /// Method declaration owned by a type.
    Method,
    /// Constructor declaration.
    Constructor,
    /// Class declaration.
    Class,
    /// Struct or record declaration.
    Struct,
    /// Enum declaration.
    Enum,
    /// Interface or protocol declaration.
    Interface,
    /// Trait or typeclass declaration.
    Trait,
    /// Implementation block.
    Impl,
    /// Module or file-level container.
    Module,
    /// Field or member variable declaration.
    Field,
    /// Property declaration.
    Property,
    /// Constant declaration.
    Constant,
    /// Variable declaration.
    Variable,
    /// Type alias or typedef declaration.
    Type,
    /// Namespace or package declaration.
    Namespace,
    /// Macro definition.
    Macro,
}

impl DeclarationRole {
    /// Returns the canonical [`crate::models::tags::NodeKind`] for this declaration role.
    pub const fn node_kind(self) -> crate::models::tags::NodeKind {
        use crate::models::tags::NodeKind;
        match self {
            Self::Function => NodeKind::Function,
            Self::Method => NodeKind::Method,
            Self::Constructor => NodeKind::Constructor,
            Self::Class => NodeKind::Class,
            Self::Struct => NodeKind::Struct,
            Self::Enum => NodeKind::Enum,
            Self::Interface => NodeKind::Interface,
            Self::Trait => NodeKind::Trait,
            Self::Impl => NodeKind::Impl,
            Self::Module => NodeKind::Module,
            Self::Field => NodeKind::Field,
            Self::Property => NodeKind::Property,
            Self::Constant => NodeKind::Constant,
            Self::Variable => NodeKind::Variable,
            Self::Type => NodeKind::Type,
            Self::Namespace => NodeKind::Namespace,
            Self::Macro => NodeKind::Macro,
        }
    }

    /// Returns the wire node-kind string for this declaration role.
    pub const fn as_str(self) -> &'static str {
        self.node_kind().as_str()
    }

    /// Parses a wire node-kind string into a typed [`DeclarationRole`], when mapped.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "constructor" => Some(Self::Constructor),
            "class" => Some(Self::Class),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            "interface" => Some(Self::Interface),
            "trait" => Some(Self::Trait),
            "impl" => Some(Self::Impl),
            "module" => Some(Self::Module),
            "field" => Some(Self::Field),
            "property" => Some(Self::Property),
            "constant" => Some(Self::Constant),
            "variable" => Some(Self::Variable),
            "type" => Some(Self::Type),
            "namespace" => Some(Self::Namespace),
            "macro" => Some(Self::Macro),
            _ => None,
        }
    }
}

/// Typed import form for declarative grammar classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImportKind {
    /// Imports a module or namespace.
    Module,
    /// Imports a named member from a module/type.
    Member,
    /// Wildcard/glob import.
    Wildcard,
    /// Static member or type import.
    Static,
    /// Preprocessor or textual include.
    Include,
    /// Relative path import.
    Relative,
}

/// Typed call form for declarative grammar classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallForm {
    /// Direct function/procedure call.
    Direct,
    /// Receiver-qualified method/member call.
    Member,
    /// Object/type construction expression.
    Constructor,
    /// Namespace/associated-path call.
    Scoped,
    /// Macro invocation.
    Macro,
}

/// Typed declaration visibility classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Exported / publicly visible.
    Public,
    /// Private to module or enclosing type.
    Private,
    /// Protected to subclasses/derived types.
    Protected,
    /// Internal to assembly/package/crate.
    Internal,
    /// Package-private visibility.
    Package,
}

/// Declarative syntax categories for profiles using the shared extractor.
#[derive(Clone, Copy, Debug, Default)]
pub struct SyntaxGrammarSpec {
    /// Grammar declaration kinds paired with indexed symbol categories.
    pub declarations: &'static [(&'static str, &'static str)],
    /// Typed declaration kinds paired with [`DeclarationRole`].
    pub declaration_roles: &'static [(&'static str, DeclarationRole)],
    /// Kinds whose expressions produce references through the call hook.
    pub references: &'static [&'static str],
    /// Typed call syntax kinds paired with [`CallForm`].
    pub call_forms: &'static [(&'static str, CallForm)],
    /// Kinds that own lexical binding summaries.
    pub scopes: &'static [&'static str],
    /// Kinds passed to the import hook.
    pub imports: &'static [&'static str],
    /// Typed import syntax kinds paired with [`ImportKind`].
    pub import_forms: &'static [(&'static str, ImportKind)],
    /// Kinds whose member expressions produce read references.
    pub member_access: &'static [&'static str],
    /// Kinds accepted as attached documentation comments.
    pub comments: &'static [&'static str],
    /// Kinds representing typed parameter/variable bindings for `GenericScopeAnalysis`.
    pub typed_bindings: &'static [&'static str],
    /// Kinds representing assignment statements/expressions for `GenericScopeAnalysis`.
    pub assignments: &'static [&'static str],
    /// Kinds representing constructor expressions.
    pub constructors: &'static [&'static str],
    /// Kinds representing member-call expressions.
    pub member_calls: &'static [&'static str],
    /// Kinds representing inheritance/heritage clauses.
    pub heritage: &'static [&'static str],
    /// Kinds introducing conditional control-flow frames.
    pub conditionals: &'static [&'static str],
    /// Visibility modifier syntax kinds paired with [`Visibility`].
    pub visibility_modifiers: &'static [(&'static str, Visibility)],
    /// Kinds passed to the mutation relation hook.
    pub mutations: &'static [&'static str],
}

impl SyntaxGrammarSpec {
    /// Empty declarative grammar spec suitable for struct update syntax in `static` items.
    pub const DEFAULT: Self = Self {
        declarations: &[],
        declaration_roles: &[],
        references: &[],
        call_forms: &[],
        scopes: &[],
        imports: &[],
        import_forms: &[],
        member_access: &[],
        comments: &[],
        typed_bindings: &[],
        assignments: &[],
        constructors: &[],
        member_calls: &[],
        heritage: &[],
        conditionals: &[],
        visibility_modifiers: &[],
        mutations: &[],
    };
}

pub(crate) struct CompiledSyntaxGrammar {
    kinds: Vec<Option<CompiledSyntaxKind>>,
    has_value_flow_selectors: bool,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct CompiledSyntaxKind {
    declaration: Option<&'static str>,
    declaration_role: Option<DeclarationRole>,
    import_kind: Option<ImportKind>,
    call_form: Option<CallForm>,
    visibility: Option<Visibility>,
    flags: u16,
}

impl CompiledSyntaxKind {
    const REFERENCE: u16 = 1 << 0;
    const SCOPE: u16 = 1 << 1;
    const IMPORT: u16 = 1 << 2;
    const MEMBER_ACCESS: u16 = 1 << 3;
    const COMMENT: u16 = 1 << 4;
    const TYPED_BINDING: u16 = 1 << 5;
    const CONSTRUCTOR: u16 = 1 << 6;
    const MEMBER_CALL: u16 = 1 << 7;
    const HERITAGE: u16 = 1 << 8;
    const CONDITIONAL: u16 = 1 << 9;
    const ASSIGNMENT: u16 = 1 << 10;
    const MUTATION: u16 = 1 << 11;

    fn has(self, flag: u16) -> bool {
        self.flags & flag != 0
    }

    pub(crate) fn declaration(self) -> Option<&'static str> {
        self.declaration
    }

    pub(crate) fn declaration_role(self) -> Option<DeclarationRole> {
        self.declaration_role
    }

    #[cfg(test)]
    pub(crate) fn import_kind(self) -> Option<ImportKind> {
        self.import_kind
    }

    #[cfg(test)]
    pub(crate) fn call_form(self) -> Option<CallForm> {
        self.call_form
    }

    #[cfg(test)]
    pub(crate) fn visibility(self) -> Option<Visibility> {
        self.visibility
    }

    pub(crate) fn is_reference(self) -> bool {
        self.has(Self::REFERENCE)
    }

    pub(crate) fn is_scope(self) -> bool {
        self.has(Self::SCOPE)
    }

    pub(crate) fn is_import(self) -> bool {
        self.has(Self::IMPORT)
    }

    pub(crate) fn is_member_access(self) -> bool {
        self.has(Self::MEMBER_ACCESS)
    }

    pub(crate) fn is_comment(self) -> bool {
        self.has(Self::COMMENT)
    }

    pub(crate) fn is_typed_binding(self) -> bool {
        self.has(Self::TYPED_BINDING)
    }

    pub(crate) fn is_assignment(self) -> bool {
        self.has(Self::ASSIGNMENT)
    }

    pub(crate) fn is_constructor(self) -> bool {
        self.has(Self::CONSTRUCTOR)
    }

    pub(crate) fn is_member_call(self) -> bool {
        self.has(Self::MEMBER_CALL)
    }

    #[cfg(test)]
    pub(crate) fn is_heritage(self) -> bool {
        self.has(Self::HERITAGE)
    }

    pub(crate) fn is_conditional(self) -> bool {
        self.has(Self::CONDITIONAL)
    }

    pub(crate) fn is_mutation(self) -> bool {
        self.has(Self::MUTATION)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DelimitedSplitError {
    UnbalancedClose(char),
    UnbalancedOpen,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DelimitedSplitOptions {
    pub(crate) delimiter: char,
    pub(crate) track_angles: bool,
    pub(crate) track_brackets: bool,
    pub(crate) track_quotes: bool,
    pub(crate) strict: bool,
}

pub(crate) fn split_top_level_delimited<'a>(
    input: &'a str,
    options: DelimitedSplitOptions,
    mut on_piece: impl FnMut(&'a str, bool) -> Result<(), &'static str>,
) -> Result<(), Result<DelimitedSplitError, &'static str>> {
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut in_quotes = false;
    let mut start = 0usize;
    let close_step = |count: &mut usize, ch: char| -> Result<(), DelimitedSplitError> {
        if options.strict {
            *count = count
                .checked_sub(1)
                .ok_or(DelimitedSplitError::UnbalancedClose(ch))?;
        } else {
            *count = count.saturating_sub(1);
        }
        Ok(())
    };
    for (idx, ch) in input.char_indices() {
        if ch == '"' && options.track_quotes {
            in_quotes = !in_quotes;
            continue;
        }
        if in_quotes {
            continue;
        }
        match ch {
            '<' if options.track_angles => angle += 1,
            '>' if options.track_angles => close_step(&mut angle, ch).map_err(Ok)?,
            '(' => paren += 1,
            ')' => close_step(&mut paren, ch).map_err(Ok)?,
            '[' if options.track_brackets => bracket += 1,
            ']' if options.track_brackets => close_step(&mut bracket, ch).map_err(Ok)?,
            _ if ch == options.delimiter && angle == 0 && paren == 0 && bracket == 0 => {
                on_piece(input[start..idx].trim(), false).map_err(Err)?;
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    if options.strict && (angle != 0 || paren != 0 || bracket != 0) {
        return Err(Ok(DelimitedSplitError::UnbalancedOpen));
    }
    on_piece(input[start..].trim(), true).map_err(Err)
}

impl CompiledSyntaxGrammar {
    pub(crate) fn classify(&self, kind_id: u16) -> Option<CompiledSyntaxKind> {
        self.kinds.get(usize::from(kind_id)).copied().flatten()
    }

    pub(crate) fn has_value_flow_selectors(&self) -> bool {
        self.has_value_flow_selectors
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct SyntaxGrammarCacheKey {
    grammar: tree_sitter::Language,
    spec_identity: usize,
}

thread_local! {
    static SYNTAX_GRAMMAR_CACHE: RefCell<HashMap<SyntaxGrammarCacheKey, Arc<CompiledSyntaxGrammar>>> = RefCell::new(HashMap::new());
}

impl SyntaxGrammarSpec {
    pub(crate) fn compile(&self, grammar: &tree_sitter::Language) -> anyhow::Result<CompiledSyntaxGrammar> {
        let kind = |name: &str| -> anyhow::Result<u16> {
            let id = grammar.id_for_node_kind(name, true);
            anyhow::ensure!(
                grammar.node_kind_for_id(id) == Some(name) && grammar.node_kind_is_named(id),
                "SyntaxGrammarSpec names unknown named kind '{name}'"
            );
            Ok(id)
        };
        let mut compiled_kinds = vec![None::<CompiledSyntaxKind>; grammar.node_kind_count()];
        let mut register_decl =
            |name: &str, category: &'static str, role: Option<DeclarationRole>| -> anyhow::Result<()> {
                anyhow::ensure!(
                    !category.is_empty(),
                    "SyntaxGrammarSpec declaration category is empty"
                );
                let compiled = compiled_kinds[usize::from(kind(name)?)].get_or_insert_default();
                anyhow::ensure!(
                    compiled.declaration.is_none(),
                    "SyntaxGrammarSpec repeats declaration kind '{name}'"
                );
                compiled.declaration = Some(category);
                compiled.declaration_role = role;
                Ok(())
            };
        for (name, category) in self.declarations {
            register_decl(name, category, DeclarationRole::parse(category))?;
        }
        for (name, role) in self.declaration_roles {
            register_decl(name, role.as_str(), Some(*role))?;
        }
        for (names, flag) in [
            (self.references, CompiledSyntaxKind::REFERENCE),
            (self.scopes, CompiledSyntaxKind::SCOPE),
            (self.imports, CompiledSyntaxKind::IMPORT),
            (self.member_access, CompiledSyntaxKind::MEMBER_ACCESS),
            (self.comments, CompiledSyntaxKind::COMMENT),
            (self.typed_bindings, CompiledSyntaxKind::TYPED_BINDING),
            (self.assignments, CompiledSyntaxKind::ASSIGNMENT),
            (self.constructors, CompiledSyntaxKind::CONSTRUCTOR),
            (self.member_calls, CompiledSyntaxKind::MEMBER_CALL),
            (self.heritage, CompiledSyntaxKind::HERITAGE),
            (self.conditionals, CompiledSyntaxKind::CONDITIONAL),
            (self.mutations, CompiledSyntaxKind::MUTATION),
        ] {
            for name in names {
                compiled_kinds[usize::from(kind(name)?)]
                    .get_or_insert_default()
                    .flags |= flag;
            }
        }
        for (name, form) in self.import_forms {
            let compiled = compiled_kinds[usize::from(kind(name)?)].get_or_insert_default();
            compiled.flags |= CompiledSyntaxKind::IMPORT;
            compiled.import_kind = Some(*form);
        }
        for (name, form) in self.call_forms {
            let compiled = compiled_kinds[usize::from(kind(name)?)].get_or_insert_default();
            compiled.flags |= CompiledSyntaxKind::REFERENCE;
            compiled.call_form = Some(*form);
            if *form == CallForm::Constructor {
                compiled.flags |= CompiledSyntaxKind::CONSTRUCTOR;
            } else if *form == CallForm::Member {
                compiled.flags |= CompiledSyntaxKind::MEMBER_CALL;
            }
        }
        for (name, visibility) in self.visibility_modifiers {
            let compiled = compiled_kinds[usize::from(kind(name)?)].get_or_insert_default();
            compiled.visibility = Some(*visibility);
        }
        let has_value_flow_selectors = !self.typed_bindings.is_empty()
            || !self.assignments.is_empty()
            || !self.constructors.is_empty()
            || !self.member_calls.is_empty()
            || !self.conditionals.is_empty()
            || self
                .call_forms
                .iter()
                .any(|(_, form)| matches!(form, CallForm::Constructor | CallForm::Member));
        Ok(CompiledSyntaxGrammar {
            kinds: compiled_kinds,
            has_value_flow_selectors,
        })
    }

    /// Reuses the immutable kind-id table for this exact static spec and grammar.
    pub(crate) fn compile_cached(
        &'static self,
        grammar: &tree_sitter::Language,
    ) -> anyhow::Result<Arc<CompiledSyntaxGrammar>> {
        let key = SyntaxGrammarCacheKey {
            grammar: grammar.clone(),
            spec_identity: self as *const Self as usize,
        };
        if let Some(compiled) = SYNTAX_GRAMMAR_CACHE.with(|cache| cache.borrow().get(&key).cloned())
        {
            return Ok(compiled);
        }

        let compiled = Arc::new(self.compile(grammar)?);
        Ok(SYNTAX_GRAMMAR_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= 128 {
                cache.clear();
            }
            cache.entry(key).or_insert_with(|| compiled.clone()).clone()
        }))
    }
}

#[derive(Clone, Debug, Default)]
/// A bounded expression shape retained for DOM listener receiver inference.
pub enum DomListenerReceiverExpression {
    /// An identifier read at a source position.
    Identifier {
        /// Identifier spelling.
        name: String,
        /// Byte offset of the identifier read.
        position: usize,
    },
    /// A field on lexical `this`.
    ThisField {
        /// Field spelling.
        name: String,
        /// Syntax identifier of the containing class.
        class_scope: usize,
        /// Whether the containing method is static.
        static_method: bool,
    },
    /// A call to a member on a recursively described receiver.
    MemberCall {
        /// Receiver expression for the member call.
        receiver: Box<DomListenerReceiverExpression>,
        /// Member spelling.
        method: String,
        /// Exact type argument spellings, if supplied.
        type_arguments: Vec<String>,
        /// Whether the call has one or more explicit arguments.
        has_arguments: bool,
    },
    /// A parenthesized receiver expression.
    Parenthesized(Box<DomListenerReceiverExpression>),
    /// Any syntax outside the bounded receiver expression grammar.
    #[default]
    Unsupported,
}

#[derive(Clone, Debug, Default)]
/// A lexical binding summary used by DOM listener receiver inference.
pub struct DomListenerLocalDeclaration {
    /// Number of same-name declarations in the lexical block.
    pub declaration_count: usize,
    /// Whether the unique declaration uses `const`.
    pub is_const: bool,
    /// End byte of the declaration statement.
    pub statement_end: usize,
    /// Bounded initializer shape for the unique declaration, when present.
    pub initializer: Option<DomListenerReceiverExpression>,
    /// Ancestor scope identifiers at the declaration site.
    pub scope_chain: Arc<Vec<usize>>,
}

#[derive(serde::Serialize)]
/// Export that refers to another module or member and resolves during linking.
pub struct LazyExport {
    /// Exported name in the current module.
    pub name: String,
    /// Module that supplies the binding.
    pub module: String,
    /// Named member, or the module itself when absent.
    pub member: Option<String>,
    /// Whether the module path is relative to the current module.
    pub relative_to_module: bool,
}

#[derive(Default)]
/// Compact semantic metadata projected into an indexed symbol.
pub struct SymbolMetadata {
    /// Extracted return type expression, when annotated or inferred by syntax rules.
    pub return_type: Option<crate::semantic::TypeExpr>,
    /// Parameter annotations keyed by parameter identifier.
    pub param_types: BTreeMap<String, String>,
    /// Explicit receiver parameter identifier (such as `self` or `cls`).
    pub receiver_name: Option<String>,
    /// Normalized decorator or attribute expressions attached to the symbol.
    pub decorators: Vec<String>,
    /// Serialized base-class or interface list declared on a nominal type.
    pub bases: Option<String>,
    /// Target type name for external or out-of-line receiver methods.
    pub receiver: Option<String>,
    /// Optional manifest provenance for rule-caused object ownership.
    pub object_ownership: Option<crate::semantic::ObjectOwnershipFact>,
    /// Normalized type annotation carried by a receiver or `impl` target.
    pub receiver_type: Option<String>,
    /// Marks callable items extracted as receiver-bound methods.
    pub is_method: Option<bool>,
    /// Marks members declared static or associated rather than instance-bound.
    pub is_static: Option<bool>,
    /// Marks default-exported JavaScript, TypeScript, or Vue declarations.
    pub default_export: bool,
    /// Marks asynchronous functions or methods.
    pub is_async: bool,
    /// Marks declaration-only overload signatures paired with an implementation.
    pub is_overload: bool,
    /// Marks declaration-only stubs (such as `.pyi` or `.d.ts` entries).
    pub is_stub: bool,
    /// Whether a declaration is guarded by source control flow in its owner scope.
    pub is_conditional: bool,
    /// Whether the declaration is public under this language's visibility rules.
    pub is_public: Option<bool>,
}

/// Borrowed syntax and source context passed to extraction hooks.
#[derive(Clone, Copy)]
pub struct SyntaxContext<'a, 'tree> {
    /// Current tree-sitter CST node being inspected.
    pub node: Syntax<'tree>,
    /// Full source text of the indexed file.
    pub source: &'a str,
    /// Owning enclosing symbol identifier or qualified scope name.
    pub owner: &'a str,
    /// Byte offset applied to extracted source coordinates.
    pub offset: usize,
    /// Borrowed typed per-language file state.
    pub file_state: &'a LanguageFileState,
    pub(crate) node_kind: Option<&'static str>,
    pub(crate) ancestors: Option<&'a [Syntax<'tree>]>,
}
impl<'a, 'tree> SyntaxContext<'a, 'tree> {
    #[inline]
    pub(crate) fn new(
        node: Syntax<'tree>,
        source: &'a str,
        owner: &'a str,
        offset: usize,
        file_state: &'a LanguageFileState,
    ) -> Self {
        Self {
            node,
            source,
            owner,
            offset,
            file_state,
            node_kind: None,
            ancestors: None,
        }
    }

    #[inline]
    pub(crate) fn with_traversal(
        node: Syntax<'tree>,
        source: &'a str,
        owner: &'a str,
        offset: usize,
        file_state: &'a LanguageFileState,
        node_kind: &'static str,
        ancestors: &'a [Syntax<'tree>],
    ) -> Self {
        Self {
            node,
            source,
            owner,
            offset,
            file_state,
            node_kind: Some(node_kind),
            ancestors: Some(ancestors),
        }
    }

    #[inline]
    pub(crate) fn kind(&self) -> &'static str {
        self.node_kind.unwrap_or_else(|| self.node.kind())
    }

    #[inline]
    pub(crate) fn ancestor(&self, depth: usize) -> Option<Syntax<'tree>> {
        if depth == 0 {
            return Some(self.node);
        }
        match self.ancestors {
            Some(ancestors) => ancestors
                .len()
                .checked_sub(depth)
                .and_then(|idx| ancestors.get(idx).copied()),
            None => {
                let mut cur = Some(self.node);
                for _ in 0..depth {
                    cur = cur?.parent();
                }
                cur
            }
        }
    }

    #[inline]
    pub(crate) fn parent(&self) -> Option<Syntax<'tree>> {
        self.ancestor(1)
    }

    /// Returns the one-based source line after applying the file offset.
    pub fn line(&self) -> usize {
        self.node.start_position().row + self.offset + 1
    }
    /// Appends an import reference at this syntax node to the file facts.
    pub fn import(
        &self,
        facts: &mut Facts,
        expression: String,
        alias: Option<String>,
        module: Option<String>,
    ) {
        facts.references.push(Reference {
            source: self.owner.into(),
            dynamic: false,
            expression,
            kind: "imports".into(),
            line: self.line(),
            column: self.node.start_position().column,
            alias,
            module,
            receiver_hint: None,
        });
    }
}

/// AST pattern-search capabilities declared by a language profile.
#[derive(Clone, Copy, Debug)]
pub struct AstSearchCapabilities {
    /// Grammar node kinds that may omit their bodies in declaration patterns.
    pub declaration_kinds: &'static [&'static str],
    /// Grammar node kinds treated as declaration bodies when the pattern omits them.
    pub declaration_body_kinds: &'static [&'static str],
    /// Suffixes the shared adapter may append to complete a bodyless declaration.
    pub declaration_completions: &'static [&'static str],
    /// Optional isolated syntax wrapper for root-rejected fragment forms.
    pub fragment_probe: Option<AstSearchProbe>,
    /// Grammar node kinds used for profile-specific attribute patterns.
    pub attribute_kinds: &'static [&'static str],
    /// Valid patterns included with unsupported-pattern diagnostics.
    pub valid_examples: &'static [&'static str],
    /// Actionable guidance included with unsupported-pattern diagnostics.
    pub diagnostic_hint: &'static str,
}

impl Default for AstSearchCapabilities {
    fn default() -> Self {
        Self {
            declaration_kinds: &[],
            declaration_body_kinds: &[],
            declaration_completions: &[],
            fragment_probe: None,
            attribute_kinds: &[],
            valid_examples: &["name"],
            diagnostic_hint: "Use complete syntax accepted by this language profile, or one of its declared fragment forms.",
        }
    }
}

/// Synthetic wrapper used to parse an isolated AST fragment.
#[derive(Clone, Copy, Debug)]
pub struct AstSearchProbe {
    /// Text inserted before the user pattern.
    pub prefix: &'static str,
    /// Text inserted after the user pattern.
    pub suffix: &'static str,
}

/// Returns the workspace-relative path with its file extension removed.
pub fn module_stem(path: &str) -> &str {
    let file_start = path.rfind('/').map_or(0, |index| index + 1);
    path[file_start..]
        .rsplit_once('.')
        .map_or(path, |(stem, _)| &path[..file_start + stem.len()])
}

pub(crate) fn match_mapping<'a>(pattern: &str, value: &'a str) -> Option<&'a str> {
    match pattern.split_once('*') {
        Some((prefix, suffix)) if !suffix.contains('*') => {
            value.strip_prefix(prefix)?.strip_suffix(suffix)
        }
        None if pattern == value => Some(""),
        _ => None,
    }
}

pub(crate) fn joined_scope(scope: &str, target: &str, floor: usize) -> Option<String> {
    if target.starts_with('/') || target.contains('\\') || target.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = scope.split('/').filter(|part| !part.is_empty()).collect();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() <= floor {
                    return None;
                }
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

pub(crate) fn javascript_namespace(path: &str) -> String {
    let (_, local_path) = workspace_path(path);
    module_stem(local_path)
        .trim_end_matches("/index")
        .replace('/', ".")
}

pub(crate) fn parse_package_declaration(source: &str, block_comments: bool) -> Option<&str> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("package")
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            let rest = rest.split_once("//").map_or(rest, |(before, _)| before);
            let rest = if block_comments {
                rest.split_once("/*").map_or(rest, |(before, _)| before)
            } else {
                rest
            };
            let pkg = rest.trim().trim_end_matches(';').trim();
            if !pkg.is_empty() {
                return Some(pkg);
            }
        }
    }
    None
}

pub use acdd_core::config::workspace_path;

/// Resolves a relative import path (`./`, `../`) against the importing module directory.
pub fn relative_namespace(owner: &str, module: &str) -> Option<String> {
    let (_, owner) = workspace_path(owner);
    let mut parts: Vec<_> = owner
        .rsplit_once('/')
        .map_or("", |(p, _)| p)
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    for part in module.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("."))
}

/// Extracts a function or macro call reference from the current syntax node.
pub fn call(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts, dynamic_imports: bool) {
    let Some(callee) =
        field(ctx.node, ctx.source, "function").or_else(|| field(ctx.node, ctx.source, "macro"))
    else {
        return;
    };
    let dynamic = dynamic_imports && matches!(callee, "import" | "require");
    let import_path = if dynamic {
        ctx.node
            .child_by_field_name("arguments")
            .and_then(|n| n.named_child(0))
            .filter(|n| n.kind() == "string")
            .map(|n| text(n, ctx.source).trim_matches(['\'', '"']).to_owned())
    } else {
        None
    };
    facts.references.push(Reference {
        source: ctx.owner.into(),
        dynamic: !callee
            .chars()
            .all(|c| c.is_alphanumeric() || "_.:!".contains(c)),
        expression: import_path
            .clone()
            .unwrap_or_else(|| bounded_expression(callee)),
        kind: if dynamic { "imports" } else { "calls" }.into(),
        line: ctx.line(),
        column: ctx.node.start_position().column,
        alias: None,
        module: import_path,
        receiver_hint: None,
    });
}
