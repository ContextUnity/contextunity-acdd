use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

macro_rules! wire_tags {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const WIRE_NAMES: &'static [&'static str] = &[$($wire),+];
            pub const fn as_str(&self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($wire => Some(Self::$variant),)+ _ => None }
            }
        }
        impl FromStr for $name {
            type Err = ();
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value).ok_or(())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
        impl From<$name> for super::compact_graph::CompactTag {
            fn from(value: $name) -> Self { Self::Static(value.as_str()) }
        }
    };
}

wire_tags!(EdgeKind {
    Calls => "calls",
    PossibleCalls => "possible_calls",
    Imports => "imports",
    Contains => "contains",
    References => "references",
    Inherits => "inherits",
    Implements => "implements",
    Bases => "bases",
    Decorates => "decorates",
    Handles => "handles",
    Mutates => "mutates",
    Registers => "registers",
    Governs => "governs",
    Documents => "documents",
    ReferencesDoc => "references_doc",
    Overrides => "overrides",
    Extends => "extends",
});

wire_tags!(EdgeConfidence {
    Exact => "exact",
    Inferred => "inferred",
    Possible => "possible",
    Heuristic => "heuristic",
});

wire_tags!(CoverageStatus {
    Resolved => "resolved",
    External => "external",
    Ambiguous => "ambiguous",
    Unresolved => "unresolved",
});

wire_tags!(NodeKind {
    Function => "function",
    Method => "method",
    Class => "class",
    Struct => "struct",
    Enum => "enum",
    Trait => "trait",
    Impl => "impl",
    Module => "module",
    Field => "field",
    Constant => "constant",
    Variable => "variable",
    Type => "type",
    Interface => "interface",
    Constructor => "constructor",
    Component => "component",
    Route => "route",
    Document => "document",
    Invariant => "invariant",
    Table => "table",
    Column => "column",
    Namespace => "namespace",
    Macro => "macro",
    Property => "property",
    TemplateScope => "template_scope",
    IslandScope => "island_scope",
    TemplateLoaderRoot => "template_loader_root",
    MainRegistration => "main_registration",
    CliCommandRegistration => "cli_command_registration",
});

wire_tags!(ReferenceKind {
    Call => "call",
    CallsEndpoint => "calls_endpoint",
    Import => "import",
    Reads => "reads",
    Writes => "writes",
    Injects => "injects",
    Renders => "renders",
    Includes => "includes",
    Member => "member",
    Extends => "extends",
    DefinesTable => "defines_table",
    ReferencesRoute => "references_route",
    SemanticNamedDefinition => "semantic_named_definition",
    SemanticNamedReference => "semantic_named_reference",
    SemanticKey => "semantic_key",
    SqlTypeQueriesTable => "sql_type_queries_table",
});

wire_tags!(LanguageId {
    Rust => "rust",
    Python => "python",
    Typescript => "typescript",
    Javascript => "javascript",
    Vue => "vue",
    Html => "html",
    Go => "go",
    Java => "java",
    Kotlin => "kotlin",
    Csharp => "csharp",
    Php => "php",
    Ruby => "ruby",
    C => "c",
    Cpp => "cpp",
    Sql => "sql",
    Proto => "proto",
    Yaml => "yaml",
    Toml => "toml",
    Markdown => "markdown",
    Workspace => "workspace",
});
