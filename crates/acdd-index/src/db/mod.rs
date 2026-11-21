//! SQLite index database storage, schema management, and query helpers.

pub mod cache;
pub mod commitments;
pub mod cycles;
pub mod lint;
pub mod paging;
pub mod reader;
pub mod schema;
pub mod symbols;
pub mod traversal;
pub mod writer;

pub use commitments::{
    read_metadata, read_metadata_optional, write_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES,
    MAX_SQLITE_VALUE_BYTES,
};
pub use schema::{initialize_code_map_schema, SCHEMA_DDL};
pub use writer::delta;
