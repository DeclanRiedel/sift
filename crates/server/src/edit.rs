//! Inline-edit → DML generation (Phase D).
//!
//! Turns an [`EditSet`] of result-grid edits into minimal, parameterized
//! `INSERT` / `UPDATE` / `DELETE` statements. Composes over `Driver::schema`
//! (for row identity + column metadata) — no new `Driver` method, so the
//! ADR-017 trait lock is undisturbed. The transactional apply lives in
//! `SessionStore::apply_edits`; this module only *generates* SQL and never
//! executes it. See `docs/PLANS/inline-edit-dml.md` (ADR-023 candidate).

use sift_driver_api::{ConnHandle, Driver};
use sift_protocol::{
    Code, DriverError, EditPlan, EditSet, EditStatement, EditStatementKind, Engine, IdentitySource,
    ObjectInfo, ObjectPath, RowEdit, SchemaScope, Value,
};

use crate::ddl::{qualified_name, quote_ident};

/// Fetch the target table's deep schema, resolve its row identity, and
/// generate the ordered, parameterized statement plan. Does not execute.
pub async fn build_plan(
    driver: &dyn Driver,
    handle: ConnHandle,
    edit_set: &EditSet,
) -> Result<EditPlan, DriverError> {
    let engine = driver.engine();
    let scope = SchemaScope::deep(edit_set.table.clone());
    let snap = driver.schema(handle, scope).await?;
    let info = find_object(&snap, &edit_set.table).ok_or_else(|| {
        DriverError::new(
            Code::UndefinedObject,
            format!("table {:?} not found in schema", edit_set.table.name),
        )
        .with_engine(engine)
    })?;
    plan_from_object(engine, info, edit_set)
}

/// Driver-free plan generation over an already-fetched table description.
/// Split out so the SQL generation is unit-testable without a live driver.
pub(crate) fn plan_from_object(
    engine: Engine,
    info: &ObjectInfo,
    edit_set: &EditSet,
) -> Result<EditPlan, DriverError> {
    let identity = resolve_identity(info, engine)?;
    let id_columns = identity_columns(&identity);
    let column_names: Vec<&str> = info.columns.iter().map(|c| c.name.as_str()).collect();

    // Order: deletes, then updates, then inserts (avoids a delete+insert of the
    // same key colliding; lets an insert reuse a key a delete just freed).
    let mut ordered: Vec<(usize, &RowEdit)> = edit_set.edits.iter().enumerate().collect();
    ordered.sort_by_key(|(_, e)| match e {
        RowEdit::Delete { .. } => 0,
        RowEdit::Update { .. } => 1,
        RowEdit::Insert { .. } => 2,
    });

    let mut statements = Vec::with_capacity(ordered.len());
    for (edit_index, edit) in ordered {
        let stmt = match edit {
            RowEdit::Insert { values } => gen_insert(
                engine,
                &edit_set.table,
                info,
                &column_names,
                values,
                id_columns,
            )?,
            RowEdit::Update {
                key,
                changes,
                expected,
            } => gen_update(
                engine,
                &edit_set.table,
                &column_names,
                id_columns,
                key,
                changes,
                expected,
            )?,
            RowEdit::Delete { key, expected } => gen_delete(
                engine,
                &edit_set.table,
                &column_names,
                id_columns,
                key,
                expected,
            )?,
        };
        statements.push(EditStatement { edit_index, ..stmt });
    }

    Ok(EditPlan {
        table: edit_set.table.clone(),
        identity,
        statements,
    })
}

/// Build a statement with a placeholder `edit_index`; the caller stamps the
/// real index via `EditStatement { edit_index, ..stmt }`.
fn raw(kind: EditStatementKind, sql: String, params: Vec<Value>) -> EditStatement {
    EditStatement {
        edit_index: 0,
        kind,
        sql,
        params,
    }
}

/// Accumulates bind params and hands back the engine-specific placeholder for
/// each. `$1..` for Postgres, `@P1..` for SQL Server (1-based).
struct Binder {
    engine: Engine,
    params: Vec<Value>,
}

impl Binder {
    fn new(engine: Engine) -> Self {
        Self {
            engine,
            params: Vec::new(),
        }
    }

    fn bind(&mut self, value: Value) -> String {
        self.params.push(value);
        let n = self.params.len();
        match self.engine {
            Engine::Postgres => format!("${n}"),
            Engine::SqlServer => format!("@P{n}"),
        }
    }
}

fn resolve_identity(info: &ObjectInfo, engine: Engine) -> Result<IdentitySource, DriverError> {
    let pk: Vec<String> = info
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.clone())
        .collect();
    if !pk.is_empty() {
        return Ok(IdentitySource::PrimaryKey { columns: pk });
    }
    // Fall back to a single non-nullable UNIQUE index.
    let nullable: std::collections::HashSet<&str> = info
        .columns
        .iter()
        .filter(|c| !matches!(c.nullable, sift_protocol::Nullability::NotNullable))
        .map(|c| c.name.as_str())
        .collect();
    if let Some(idx) = info.indexes.iter().find(|i| {
        i.unique
            && !i.columns.is_empty()
            && i.columns.iter().all(|c| !nullable.contains(c.as_str()))
    }) {
        return Ok(IdentitySource::UniqueIndex {
            name: idx.name.clone(),
            columns: idx.columns.clone(),
        });
    }
    Err(DriverError::new(
        Code::EditNoRowIdentity,
        format!(
            "table {:?} has no primary key or non-nullable unique index; \
             inline edits need a stable row identity",
            info.name
        ),
    )
    .with_engine(engine))
}

fn identity_columns(identity: &IdentitySource) -> &[String] {
    match identity {
        IdentitySource::PrimaryKey { columns } => columns,
        IdentitySource::UniqueIndex { columns, .. } => columns,
    }
}

fn ensure_column(columns: &[&str], name: &str, engine: Engine) -> Result<(), DriverError> {
    if columns.contains(&name) {
        Ok(())
    } else {
        Err(DriverError::new(
            Code::InvalidParameterValue,
            format!("column {name:?} does not exist on the target table"),
        )
        .with_engine(engine))
    }
}

fn is_db_assigned(info: &ObjectInfo, name: &str) -> bool {
    info.columns.iter().any(|c| {
        c.name == name
            && (c.auto_increment || c.facets.postgres.as_ref().is_some_and(|p| p.is_identity))
    })
}

/// Build the identity `WHERE` predicate plus optional optimistic `expected`
/// predicates. Returns the SQL fragment (without the leading `WHERE`).
fn where_clause(
    binder: &mut Binder,
    engine: Engine,
    columns: &[&str],
    id_columns: &[String],
    key: &sift_protocol::RowKey,
    expected: &[sift_protocol::CellEdit],
) -> Result<String, DriverError> {
    let mut preds: Vec<String> = Vec::new();
    // Identity columns are mandatory and pulled from the key by name so a
    // client can't target a non-identity column set.
    for id_col in id_columns {
        let cell = key
            .columns
            .iter()
            .find(|c| &c.column == id_col)
            .ok_or_else(|| {
                DriverError::new(
                    Code::InvalidParameterValue,
                    format!("row key is missing identity column {id_col:?}"),
                )
                .with_engine(engine)
            })?;
        preds.push(comparison(binder, engine, id_col, &cell.value));
    }
    // Optimistic-concurrency predicates: only meaningful for real columns.
    for cell in expected {
        ensure_column(columns, &cell.column, engine)?;
        preds.push(comparison(binder, engine, &cell.column, &cell.value));
    }
    Ok(preds.join(" AND "))
}

/// `col = $n`, or `col IS NULL` for a NULL comparison (no bind).
fn comparison(binder: &mut Binder, engine: Engine, column: &str, value: &Value) -> String {
    let ident = quote_ident(column, engine);
    if value.is_null() {
        format!("{ident} IS NULL")
    } else {
        format!("{ident} = {}", binder.bind(value.clone()))
    }
}

/// `RETURNING`/`OUTPUT` clause naming the identity columns, so an insert can
/// hand back a database-assigned key.
fn returning_clause(engine: Engine, id_columns: &[String], position: ReturningPos) -> String {
    let cols: Vec<String> = id_columns.iter().map(|c| quote_ident(c, engine)).collect();
    match (engine, position) {
        (Engine::Postgres, ReturningPos::Trailing) => format!(" RETURNING {}", cols.join(", ")),
        (Engine::SqlServer, ReturningPos::Output) => {
            let outs: Vec<String> = id_columns
                .iter()
                .map(|c| format!("inserted.{}", quote_ident(c, engine)))
                .collect();
            format!(" OUTPUT {}", outs.join(", "))
        }
        _ => String::new(),
    }
}

enum ReturningPos {
    Trailing,
    Output,
}

fn gen_insert(
    engine: Engine,
    table: &ObjectPath,
    info: &ObjectInfo,
    columns: &[&str],
    values: &[sift_protocol::CellEdit],
    id_columns: &[String],
) -> Result<EditStatement, DriverError> {
    let mut binder = Binder::new(engine);
    let mut col_idents = Vec::new();
    let mut placeholders = Vec::new();
    for cell in values {
        ensure_column(columns, &cell.column, engine)?;
        // Omit database-assigned columns; the DB fills them.
        if is_db_assigned(info, &cell.column) {
            continue;
        }
        col_idents.push(quote_ident(&cell.column, engine));
        placeholders.push(binder.bind(cell.value.clone()));
    }
    let table_sql = qualified_name(table, engine);
    let sql = if col_idents.is_empty() {
        // No user-supplied columns (all db-assigned) — emit engine default row.
        match engine {
            Engine::Postgres => format!(
                "INSERT INTO {table_sql} DEFAULT VALUES{}",
                returning_clause(engine, id_columns, ReturningPos::Trailing)
            ),
            Engine::SqlServer => format!(
                "INSERT INTO {table_sql}{} DEFAULT VALUES",
                returning_clause(engine, id_columns, ReturningPos::Output)
            ),
        }
    } else {
        let cols = col_idents.join(", ");
        let vals = placeholders.join(", ");
        match engine {
            Engine::Postgres => format!(
                "INSERT INTO {table_sql} ({cols}) VALUES ({vals}){}",
                returning_clause(engine, id_columns, ReturningPos::Trailing)
            ),
            Engine::SqlServer => format!(
                "INSERT INTO {table_sql} ({cols}){} VALUES ({vals})",
                returning_clause(engine, id_columns, ReturningPos::Output)
            ),
        }
    };
    Ok(raw(EditStatementKind::Insert, sql, binder.params))
}

fn gen_update(
    engine: Engine,
    table: &ObjectPath,
    columns: &[&str],
    id_columns: &[String],
    key: &sift_protocol::RowKey,
    changes: &[sift_protocol::CellEdit],
    expected: &[sift_protocol::CellEdit],
) -> Result<EditStatement, DriverError> {
    if changes.is_empty() {
        return Err(DriverError::new(
            Code::InvalidParameterValue,
            "update edit has no changed columns",
        )
        .with_engine(engine));
    }
    let mut binder = Binder::new(engine);
    let mut sets = Vec::new();
    for cell in changes {
        ensure_column(columns, &cell.column, engine)?;
        let ident = quote_ident(&cell.column, engine);
        let placeholder = binder.bind(cell.value.clone());
        sets.push(format!("{ident} = {placeholder}"));
    }
    let where_sql = where_clause(&mut binder, engine, columns, id_columns, key, expected)?;
    let table_sql = qualified_name(table, engine);
    let sql = format!(
        "UPDATE {table_sql} SET {} WHERE {where_sql}",
        sets.join(", ")
    );
    Ok(raw(EditStatementKind::Update, sql, binder.params))
}

fn gen_delete(
    engine: Engine,
    table: &ObjectPath,
    columns: &[&str],
    id_columns: &[String],
    key: &sift_protocol::RowKey,
    expected: &[sift_protocol::CellEdit],
) -> Result<EditStatement, DriverError> {
    let mut binder = Binder::new(engine);
    let where_sql = where_clause(&mut binder, engine, columns, id_columns, key, expected)?;
    let table_sql = qualified_name(table, engine);
    let sql = format!("DELETE FROM {table_sql} WHERE {where_sql}");
    Ok(raw(EditStatementKind::Delete, sql, binder.params))
}

fn find_object<'a>(
    snap: &'a sift_protocol::SchemaSnapshot,
    path: &ObjectPath,
) -> Option<&'a ObjectInfo> {
    snap.trees
        .iter()
        .flat_map(|t| t.schemas.iter())
        .filter(|s| path.schema.as_deref().map_or(true, |want| s.name == want))
        .flat_map(|s| s.objects.iter())
        .find(|o| o.name == path.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{
        CellEdit, ColumnMetadata, IndexInfo, IndexKind, Nullability, ObjectKind, PrimitiveType,
        RowKey, TypeRef,
    };

    fn col(name: &str, pk: bool, auto: bool, nullable: Nullability) -> ColumnMetadata {
        let mut c = ColumnMetadata::new(name, TypeRef::Primitive(PrimitiveType::Text));
        c.primary_key = pk;
        c.auto_increment = auto;
        c.nullable = nullable;
        c
    }

    fn table(cols: Vec<ColumnMetadata>) -> ObjectInfo {
        let mut o = ObjectInfo::new("users", ObjectKind::Table);
        o.columns = cols;
        o
    }

    fn cell(name: &str, v: Value) -> CellEdit {
        CellEdit {
            column: name.into(),
            value: v,
        }
    }

    fn eset(edits: Vec<RowEdit>) -> EditSet {
        let mut p = ObjectPath::new("users");
        p.schema = Some("public".into());
        EditSet { table: p, edits }
    }

    fn plan(engine: Engine, info: &ObjectInfo, edits: Vec<RowEdit>) -> EditPlan {
        plan_from_object(engine, info, &eset(edits)).unwrap()
    }

    #[test]
    fn update_pk_builds_parameterized_where_pg() {
        let t = table(vec![
            col("id", true, false, Nullability::NotNullable),
            col("email", false, false, Nullability::Nullable),
        ]);
        let p = plan(
            Engine::Postgres,
            &t,
            vec![RowEdit::Update {
                key: RowKey {
                    columns: vec![cell("id", Value::Int32(1))],
                },
                changes: vec![cell("email", Value::Text("new@x".into()))],
                expected: vec![cell("email", Value::Text("old@x".into()))],
            }],
        );
        let s = &p.statements[0];
        assert_eq!(s.kind, EditStatementKind::Update);
        assert_eq!(
            s.sql,
            r#"UPDATE "public"."users" SET "email" = $1 WHERE "id" = $2 AND "email" = $3"#
        );
        assert_eq!(
            s.params,
            vec![
                Value::Text("new@x".into()),
                Value::Int32(1),
                Value::Text("old@x".into())
            ]
        );
    }

    #[test]
    fn composite_pk_where_covers_all_key_columns() {
        let t = table(vec![
            col("tenant", true, false, Nullability::NotNullable),
            col("id", true, false, Nullability::NotNullable),
            col("name", false, false, Nullability::Nullable),
        ]);
        let p = plan(
            Engine::Postgres,
            &t,
            vec![RowEdit::Delete {
                key: RowKey {
                    columns: vec![cell("tenant", Value::Int32(7)), cell("id", Value::Int32(9))],
                },
                expected: vec![],
            }],
        );
        assert_eq!(
            p.statements[0].sql,
            r#"DELETE FROM "public"."users" WHERE "tenant" = $1 AND "id" = $2"#
        );
    }

    #[test]
    fn unique_index_fallback_when_no_pk() {
        let mut t = table(vec![
            col("sku", false, false, Nullability::NotNullable),
            col("name", false, false, Nullability::Nullable),
        ]);
        t.indexes = vec![IndexInfo {
            name: "uq_sku".into(),
            columns: vec!["sku".into()],
            unique: true,
            primary_key: false,
            kind: IndexKind::Btree,
            partial_predicate: None,
        }];
        let p = plan_from_object(Engine::Postgres, &t, &eset(vec![])).unwrap();
        assert!(matches!(p.identity, IdentitySource::UniqueIndex { .. }));
    }

    #[test]
    fn no_identity_is_rejected() {
        let t = table(vec![col("name", false, false, Nullability::Nullable)]);
        let err = plan_from_object(Engine::Postgres, &t, &eset(vec![])).unwrap_err();
        assert_eq!(err.code, Code::EditNoRowIdentity);
    }

    #[test]
    fn insert_omits_auto_increment_and_returns_key() {
        let t = table(vec![
            col("id", true, true, Nullability::NotNullable),
            col("email", false, false, Nullability::NotNullable),
        ]);
        let p = plan(
            Engine::Postgres,
            &t,
            vec![RowEdit::Insert {
                values: vec![
                    cell("id", Value::Int32(999)),
                    cell("email", Value::Text("a@b".into())),
                ],
            }],
        );
        let s = &p.statements[0];
        assert_eq!(
            s.sql,
            r#"INSERT INTO "public"."users" ("email") VALUES ($1) RETURNING "id""#
        );
        assert_eq!(s.params, vec![Value::Text("a@b".into())]);
    }

    #[test]
    fn null_expected_becomes_is_null() {
        let t = table(vec![
            col("id", true, false, Nullability::NotNullable),
            col("email", false, false, Nullability::Nullable),
        ]);
        let p = plan(
            Engine::Postgres,
            &t,
            vec![RowEdit::Delete {
                key: RowKey {
                    columns: vec![cell("id", Value::Int32(1))],
                },
                expected: vec![cell("email", Value::Null)],
            }],
        );
        assert_eq!(
            p.statements[0].sql,
            r#"DELETE FROM "public"."users" WHERE "id" = $1 AND "email" IS NULL"#
        );
        // Only the id is bound; the NULL comparison carries no param.
        assert_eq!(p.statements[0].params, vec![Value::Int32(1)]);
    }

    #[test]
    fn statements_ordered_delete_update_insert() {
        let t = table(vec![
            col("id", true, false, Nullability::NotNullable),
            col("name", false, false, Nullability::Nullable),
        ]);
        let p = plan(
            Engine::Postgres,
            &t,
            vec![
                RowEdit::Insert {
                    values: vec![cell("name", Value::Text("i".into()))],
                },
                RowEdit::Delete {
                    key: RowKey {
                        columns: vec![cell("id", Value::Int32(1))],
                    },
                    expected: vec![],
                },
                RowEdit::Update {
                    key: RowKey {
                        columns: vec![cell("id", Value::Int32(2))],
                    },
                    changes: vec![cell("name", Value::Text("u".into()))],
                    expected: vec![],
                },
            ],
        );
        let kinds: Vec<_> = p.statements.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EditStatementKind::Delete,
                EditStatementKind::Update,
                EditStatementKind::Insert
            ]
        );
        // edit_index maps back to the original position.
        assert_eq!(p.statements[0].edit_index, 1); // delete was index 1
        assert_eq!(p.statements[1].edit_index, 2); // update was index 2
        assert_eq!(p.statements[2].edit_index, 0); // insert was index 0
    }

    #[test]
    fn mssql_uses_bracket_quoting_and_at_p_placeholders() {
        let t = table(vec![
            col("id", true, false, Nullability::NotNullable),
            col("email", false, false, Nullability::Nullable),
        ]);
        let p = plan(
            Engine::SqlServer,
            &t,
            vec![RowEdit::Update {
                key: RowKey {
                    columns: vec![cell("id", Value::Int32(1))],
                },
                changes: vec![cell("email", Value::Text("x".into()))],
                expected: vec![],
            }],
        );
        assert_eq!(
            p.statements[0].sql,
            "UPDATE [public].[users] SET [email] = @P1 WHERE [id] = @P2"
        );
    }

    #[test]
    fn mssql_insert_places_output_before_values() {
        let t = table(vec![
            col("id", true, true, Nullability::NotNullable),
            col("email", false, false, Nullability::NotNullable),
        ]);
        let p = plan(
            Engine::SqlServer,
            &t,
            vec![RowEdit::Insert {
                values: vec![cell("email", Value::Text("a@b".into()))],
            }],
        );
        assert_eq!(
            p.statements[0].sql,
            "INSERT INTO [public].[users] ([email]) OUTPUT inserted.[id] VALUES (@P1)"
        );
    }

    #[test]
    fn unknown_column_is_rejected() {
        let t = table(vec![col("id", true, false, Nullability::NotNullable)]);
        let err = plan_from_object(
            Engine::Postgres,
            &t,
            &eset(vec![RowEdit::Update {
                key: RowKey {
                    columns: vec![cell("id", Value::Int32(1))],
                },
                changes: vec![cell("nope", Value::Text("x".into()))],
                expected: vec![],
            }]),
        )
        .unwrap_err();
        assert_eq!(err.code, Code::InvalidParameterValue);
    }
}
