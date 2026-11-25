//! Database-scoped metadata without changing the owning session's database.

/// SQL Server runs the system procedure in the named database and restores the
/// caller's context on return, including errors. Values remain separately bound.
pub fn database_query(database: &str, sql: &str) -> String {
    format!(
        "EXEC [{}].sys.sp_executesql N'{}'",
        database.replace(']', "]]"),
        sql.replace('\'', "''")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_and_batch_have_separate_escaping() {
        assert_eq!(
            database_query("db]'name", "SELECT N'quoted'"),
            "EXEC [db]]'name].sys.sp_executesql N'SELECT N''quoted'''"
        );
    }
}
