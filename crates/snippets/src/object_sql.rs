//! Reviewable editor templates, never authoritative catalog definitions or executed DDL.

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn ddl_quote_identifier(provider_id: &sift_protocol::ProviderId, identifier: &str) -> String {
    if provider_id.as_str() == "sift/sql-server" {
        format!("[{}]", identifier.replace(']', "]]"))
    } else {
        quote_identifier(identifier)
    }
}

pub fn object_designer_sql(
    provider_id: &sift_protocol::ProviderId,
    schema: &str,
    object: &str,
    kind: sift_protocol::ObjectKind,
) -> Option<String> {
    let postgres = match provider_id.as_str() {
        "sift/postgres" => true,
        "sift/sql-server" => false,
        _ => return None,
    };
    let schema = ddl_quote_identifier(provider_id, schema);
    let object = ddl_quote_identifier(provider_id, object);
    let qualified = format!("{schema}.{object}");
    match kind {
        sift_protocol::ObjectKind::Sequence => Some(if postgres {
            format!("-- Review the current sequence DDL before execution.\nALTER SEQUENCE {qualified}\n    INCREMENT BY 1\n    NO MINVALUE\n    NO MAXVALUE\n    CACHE 1;\n")
        } else {
            format!("-- Review the current sequence DDL before execution.\nALTER SEQUENCE {qualified}\n    INCREMENT BY 1\n    NO CACHE;\n")
        }),
        sift_protocol::ObjectKind::Trigger => Some(if postgres {
            format!("-- Replace the function and timing/event after reviewing the object DDL.\nCREATE OR REPLACE TRIGGER {object}\n    BEFORE INSERT ON {schema}.replace_table\n    FOR EACH ROW\n    EXECUTE FUNCTION {schema}.replace_trigger_function();\n")
        } else {
            format!("-- Replace the table and body after reviewing the object DDL.\nCREATE OR ALTER TRIGGER {qualified}\nON {schema}.replace_table\nAFTER INSERT\nAS\nBEGIN\n    SET NOCOUNT ON;\nEND;\n")
        }),
        sift_protocol::ObjectKind::Type => Some(if postgres {
            format!("-- PostgreSQL enum evolution is additive; replace the value deliberately.\nALTER TYPE {qualified} ADD VALUE 'new_value';\n")
        } else {
            format!("-- SQL Server alias types cannot be altered in place. Create a replacement,\n-- migrate dependants, then drop the old type after review.\nCREATE TYPE {schema}.replace_type FROM nvarchar(255) NULL;\n")
        }),
        _ => None,
    }
}

pub fn table_preview_sql(
    provider_id: &sift_protocol::ProviderId,
    schema: &str,
    object: &str,
) -> Option<String> {
    let qualified = format!(
        "{}.{}",
        ddl_quote_identifier(provider_id, schema),
        ddl_quote_identifier(provider_id, object)
    );
    match provider_id.as_str() {
        "sift/postgres" => Some(format!("SELECT * FROM {qualified} LIMIT 100;")),
        "sift/sql-server" => Some(format!("SELECT TOP (100) * FROM {qualified};")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{Engine, ObjectKind, ProviderId};

    #[test]
    fn previews_quote_each_engine_and_unknown_providers_fail_closed() {
        assert_eq!(
            table_preview_sql(&Engine::Postgres.provider_id(), "odd\"schema", "table").unwrap(),
            "SELECT * FROM \"odd\"\"schema\".\"table\" LIMIT 100;"
        );
        assert_eq!(
            table_preview_sql(&Engine::SqlServer.provider_id(), "dbo", "odd]table").unwrap(),
            "SELECT TOP (100) * FROM [dbo].[odd]]table];"
        );
        let unknown = ProviderId::new("thirdparty/postgres-like").unwrap();
        assert!(table_preview_sql(&unknown, "public", "table").is_none());
        assert!(
            object_designer_sql(&unknown, "public", "sequence", ObjectKind::Sequence).is_none()
        );
    }
}
