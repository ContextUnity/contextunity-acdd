pub(crate) use crate::languages::types::Ecosystem;

/// Workspace-relative module namespace prefix declared by a dependency manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ModulePrefix {
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
pub(crate) struct ParsedManifest {
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
pub(crate) trait ManifestParser: Send + Sync {
    /// Ecosystem handled by this parser.
    #[cfg_attr(not(test), allow(dead_code))]
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
