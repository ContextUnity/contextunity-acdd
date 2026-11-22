use super::super::LanguageFamily;
use super::parsers;

/// Supported language ecosystems for modular dependency manifest parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ecosystem {
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
    pub fn from_family(family: &str) -> Option<Self> {
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
    pub const fn families(self) -> &'static [LanguageFamily] {
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

/// Workspace-relative module namespace prefix declared by a dependency manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulePrefix {
    /// Ecosystem owning this prefix.
    pub ecosystem: Ecosystem,
    /// Declared namespace prefix (e.g. `example.com/app`, `App`, `Acme.Billing`).
    pub prefix: String,
    /// Directory root this prefix maps to (relative to manifest during parsing, workspace-relative in registry).
    pub source_root: String,
    /// Manifest origin path that declared this prefix.
    pub origin: String,
}

/// Candidate entrypoint declared inside a dependency manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedEntrypoint {
    pub name: String,
    pub target: String,
    pub python: bool,
}

/// Cargo crate library/target metadata extracted alongside dependencies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RustManifestInfo {
    pub package_library: Option<String>,
    pub root_crate: Option<String>,
}

/// Typed output produced by a [`ManifestParser`] plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedManifest {
    /// Ecosystem owning this manifest.
    pub ecosystem: Ecosystem,
    /// External dependencies declared by the manifest.
    pub dependencies: Vec<String>,
    /// Own package identities declared by the manifest.
    pub own_packages: Vec<String>,
    /// Module namespace prefixes declared by the manifest.
    pub module_prefixes: Vec<ModulePrefix>,
    /// Candidate entrypoints declared by the manifest.
    pub(crate) entrypoints: Vec<ParsedEntrypoint>,
    /// Optional Cargo library metadata.
    pub(crate) rust_info: Option<RustManifestInfo>,
    /// True for workspace/path-config manifests (e.g. `tsconfig*.json`, `pnpm-workspace.yaml`).
    pub(crate) skip_dependency_recording: bool,
}

impl ParsedManifest {
    pub(crate) fn new(ecosystem: Ecosystem) -> Self {
        Self {
            ecosystem,
            dependencies: Vec::new(),
            own_packages: Vec::new(),
            module_prefixes: Vec::new(),
            entrypoints: Vec::new(),
            rust_info: None,
            skip_dependency_recording: false,
        }
    }

    pub(crate) fn finish(mut self) -> Self {
        self.dependencies.sort_unstable();
        self.dependencies.dedup();
        self.module_prefixes.sort_unstable();
        self.module_prefixes.dedup();
        self
    }
}

/// Trait implemented by ecosystem-specific dependency manifest parsers.
pub trait ManifestParser: Send + Sync {
    /// Ecosystem handled by this parser.
    fn ecosystem(&self) -> Ecosystem;

    /// Exact filenames or wildcard patterns (`*.ext`, `prefix*.ext`) recognized by this parser.
    fn filenames(&self) -> &'static [&'static str];

    /// Reports whether `filename` (basename) matches any pattern owned by this parser.
    fn matches_filename(&self, filename: &str) -> bool {
        self.filenames()
            .iter()
            .any(|pattern| filename_matches(pattern, filename))
    }

    /// Parses manifest `content` for basename `filename` into a typed [`ParsedManifest`].
    fn parse(&self, filename: &str, content: &str) -> ParsedManifest;
}

/// Registered ecosystem manifest parsers evaluated during workspace discovery.
pub static MANIFEST_PARSERS: &[&'static dyn ManifestParser] = &[
    &parsers::cargo::CARGO_PARSER,
    &parsers::npm::NPM_PARSER,
    &parsers::python::PYTHON_PARSER,
    &parsers::go::GO_PARSER,
    &parsers::jvm::JVM_PARSER,
    &parsers::php::PHP_PARSER,
    &parsers::csharp::CSHARP_PARSER,
    &parsers::ruby::RUBY_PARSER,
    &parsers::c_cpp::C_CPP_PARSER,
];

/// Matches an exact filename or single-wildcard (`*`) filename pattern.
pub(crate) fn filename_matches(pattern: &str, filename: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            filename.len() >= prefix.len() + suffix.len()
                && filename.starts_with(prefix)
                && filename.ends_with(suffix)
        }
        None => pattern == filename,
    }
}
