use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

macro_rules! wire_tags {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[repr(u8)]
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
    };
}

wire_tags!(EdgeKind {
    Calls => "calls",
    PossibleCalls => "possible_calls",
    Imports => "imports",
    Contains => "contains",
    DefinesTable => "defines_table",
    QueriesTable => "queries_table",
    MutatesTable => "mutates_table",
    ReferencesTable => "references_table",
    UsesModel => "uses_model",
    CallsEndpoint => "calls_endpoint",
    ReferencesRoute => "references_route",
    RendersTemplate => "renders_template",
    InjectsDependency => "injects_dependency",
    References => "references",
    Inherits => "inherits",
    Implements => "implements",
    Bases => "bases",
    Decorates => "decorates",
    Handles => "handles",
    Mutates => "mutates",
    Registers => "registers",
    Injects => "injects",
    Renders => "renders",
    ContextProvider => "context_provider",
    Includes => "includes",
    TemplateImports => "template_imports",
    Governs => "governs",
    Documents => "documents",
    ReferencesDoc => "references_doc",
    CitesDoc => "cites_doc",
    CitedByDoc => "cited_by_doc",
    GovernedBy => "governed_by",
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
    ModuleDeclaration => "module_declaration",
    View => "view",
    Service => "service",
    ToolRegistration => "tool_registration",
    EventHandlerRegistration => "event_handler_registration",
    HtmlRequest => "html_request",
    HtmlContext => "html_context",
    Object => "object",
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
    TemplateImports => "template_imports",
    TemplateLoads => "template_loads",
    TemplateBindings => "template_bindings",
    QueriesTable => "queries_table",
    MutatesTable => "mutates_table",
    ReferencesTable => "references_table",
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
    Bash => "bash",
    Css => "css",
    Json => "json",
    Markdown => "markdown",
    Workspace => "workspace",
});

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_copy<T: Copy>() {}

    #[test]
    fn graph_tags_are_closed_one_byte_copy_enums() {
        assert_copy::<NodeKind>();
        assert_copy::<LanguageId>();
        assert_copy::<EdgeKind>();
        assert_copy::<EdgeConfidence>();
        assert_copy::<CoverageStatus>();
        assert_copy::<ReferenceKind>();

        assert_eq!(std::mem::size_of::<NodeKind>(), 1);
        assert_eq!(std::mem::size_of::<LanguageId>(), 1);
        assert_eq!(std::mem::size_of::<EdgeKind>(), 1);
        assert_eq!(std::mem::size_of::<EdgeConfidence>(), 1);
        assert_eq!(std::mem::size_of::<CoverageStatus>(), 1);
        assert_eq!(std::mem::size_of::<ReferenceKind>(), 1);

        assert_eq!(NodeKind::ALL.len(), 36);
        assert_eq!(EdgeKind::ALL.len(), 34);
        assert_eq!(ReferenceKind::ALL.len(), 22);
        for (tag, wire) in NodeKind::ALL.iter().zip(NodeKind::WIRE_NAMES) {
            assert_eq!(tag.as_str(), *wire);
            assert_eq!(NodeKind::parse(wire), Some(*tag));
        }
        for (tag, wire) in EdgeKind::ALL.iter().zip(EdgeKind::WIRE_NAMES) {
            assert_eq!(tag.as_str(), *wire);
            assert_eq!(EdgeKind::parse(wire), Some(*tag));
        }
        for (tag, wire) in ReferenceKind::ALL.iter().zip(ReferenceKind::WIRE_NAMES) {
            assert_eq!(tag.as_str(), *wire);
            assert_eq!(ReferenceKind::parse(wire), Some(*tag));
        }
    }
}
