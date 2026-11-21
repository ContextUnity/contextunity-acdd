//! Scoped ownership of SQLite temporary tables.
use super::batch::MultiValueBatch;
use anyhow::Result;
use rusqlite::Connection;
use rusqlite::ToSql;

/// Drops a temporary table when its scope ends.
pub struct TempTableGuard<'a> {
    conn: &'a Connection,
    name: &'static str,
}

impl<'a> TempTableGuard<'a> {
    /// Creates a temporary table from a trusted static name and SQL schema.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite cannot create the table.
    pub fn new(conn: &'a Connection, name: &'static str, schema: &str) -> Result<Self> {
        conn.execute_batch(&format!("CREATE TEMP TABLE {name}{schema}"))?;
        Ok(Self { conn, name })
    }

    pub(super) fn batch<const COLUMNS: usize, T: ToSql>(
        &self,
        prefix: &'static str,
    ) -> Result<MultiValueBatch<'_, COLUMNS, T>> {
        MultiValueBatch::new(self.conn, prefix)
    }
}

impl Drop for TempTableGuard<'_> {
    fn drop(&mut self) {
        let _ = self
            .conn
            .execute_batch(&format!("DROP TABLE IF EXISTS temp.{}", self.name));
    }
}
