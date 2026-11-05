use sift_protocol::{SqlServerIndexAction as Action, SqlServerIndexRequest};
pub fn sql(request: &SqlServerIndexRequest) -> Result<String, &'static str> {
    fn quote(name: &str) -> Result<String, &'static str> {
        if name.is_empty()
            || name.encode_utf16().count() > 128
            || name.chars().any(char::is_control)
        {
            return Err("Index maintenance names require 1..128 UTF-16 units without controls");
        }
        Ok(format!("[{}]", name.replace(']', "]]")))
    }
    let target = format!("{}.{}", quote(&request.schema)?, quote(&request.table)?);
    let index = quote(&request.index)?;
    Ok(match request.action {
        Action::Rebuild => {
            format!("ALTER INDEX {index} ON {target} REBUILD WITH (ONLINE = OFF, MAXDOP = 1)")
        }
        Action::Reorganize => format!("ALTER INDEX {index} ON {target} REORGANIZE"),
        Action::UpdateStatistics => {
            format!("UPDATE STATISTICS {target} {index} WITH FULLSCAN, MAXDOP = 1")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sysname_limits_count_utf16_and_quotes_never_create_all_index_commands() {
        let mut request = SqlServerIndexRequest {
            schema: "dbo".into(),
            table: "table] name".into(),
            index: "ALL]; DROP TABLE t;--".into(),
            action: Action::Rebuild,
            apply: false,
            production_confirmed: false,
            backup_verified: false,
            precondition: None,
        };
        assert_eq!(sql(&request).unwrap(),"ALTER INDEX [ALL]]; DROP TABLE t;--] ON [dbo].[table]] name] REBUILD WITH (ONLINE = OFF, MAXDOP = 1)");
        request.index = "😀".repeat(64);
        assert!(sql(&request).is_ok());
        request.index.push('😀');
        assert!(sql(&request).is_err());
        request.index = "\0".into();
        assert!(sql(&request).is_err());
    }
}
