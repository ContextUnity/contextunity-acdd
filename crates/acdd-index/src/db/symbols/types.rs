use crate::db::reader::types as reader_types;
use acdd_core::response::{CoverageOptions, QueryOptions, SourceOptions};

/// Filters and paging for a symbol search.
pub struct SearchOptions<'a> {
    /// Optional symbol kind.
    pub kind: Option<&'a str>,
    /// Workspace-relative file or directory scope.
    pub path: Option<&'a str>,
    /// Whether Markdown document symbols are included.
    pub include_docs: bool,
    /// Whether the name or qualified name must match exactly.
    pub exact: bool,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Output controls for symbol inspection.
pub struct InspectOptions<'a> {
    /// Whether linked documentation is included.
    pub show_doc: bool,
    /// Optional bounded source preview.
    pub source: &'a SourceOptions,
    /// Optional resolution coverage.
    pub coverage: CoverageOptions,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Output controls for symbol explanation.
pub struct SymbolExplainOptions<'a> {
    /// Core explanation direction, documentation inclusion, and paging contract.
    pub explain: reader_types::ExplainOptions<'a>,
    /// Optional bounded source preview.
    pub source: &'a SourceOptions,
    /// Optional resolution coverage.
    pub coverage: CoverageOptions,
}
