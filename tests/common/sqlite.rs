use rusqlite::{backup::Backup, Connection, Params};
use std::sync::Mutex;

/// Copies a persisted SQLite baseline into independent in-memory query connections
/// so shared fixtures drop temporary workspaces immediately without leaking directories.
#[allow(dead_code)]
pub struct PersistedSnapshot(Mutex<Connection>);

#[allow(dead_code)]
impl PersistedSnapshot {
    pub fn from_connection(source: &Connection) -> Self {
        Self(Mutex::new(Self::copy(source)))
    }

    pub fn open(&self) -> Connection {
        Self::copy(&self.0.lock().unwrap())
    }

    fn copy(source: &Connection) -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        Backup::new(source, &mut connection)
            .unwrap()
            .run_to_completion(256, std::time::Duration::ZERO, None)
            .unwrap();
        connection
    }
}

/// Reads the first column of every row. The statement stays on this connection.
pub fn query_strings(connection: &Connection, sql: &str, params: impl Params) -> Vec<String> {
    connection
        .prepare_cached(sql)
        .unwrap_or_else(|error| panic!("could not prepare test query {sql}: {error}"))
        .query_map(params, |row| row.get(0))
        .unwrap_or_else(|error| panic!("could not run test query {sql}: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("could not read test query {sql}: {error}"))
}

/// Reads one scalar. The query must return exactly one row.
pub fn query_value<T: rusqlite::types::FromSql>(
    connection: &Connection,
    sql: &str,
    params: impl Params,
) -> T {
    connection
        .query_row(sql, params, |row| row.get(0))
        .unwrap_or_else(|error| panic!("could not read test scalar {sql}: {error}"))
}
