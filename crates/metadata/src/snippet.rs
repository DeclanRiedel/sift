use rusqlite::{params, OptionalExtension as _, TransactionBehavior};
use sift_protocol::{DialectId, SnippetId, SnippetScope, SqlSnippet};

use crate::{MetadataError, MetadataStore, PrincipalId, Result, TenantId};

#[derive(Debug, Clone, Copy)]
pub struct SnippetWriteAuthorization {
    pub tenant: TenantId,
    pub actor: PrincipalId,
    pub tenant_admin: bool,
    pub editable_workspace: Option<i64>,
}

impl MetadataStore {
    pub fn create_sql_snippet(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        mut snippet: SqlSnippet,
    ) -> Result<SqlSnippet> {
        let scope = stored_scope(snippet.scope)?;
        let owner_id = (snippet.scope == SnippetScope::Personal).then_some(owner.0);
        let workspace_id = (snippet.scope == SnippetScope::Workspace)
            .then_some(snippet.workspace_id)
            .flatten();
        let dialects = serde_json::to_string(&snippet.dialects)?;
        let now = crate::now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sql_snippet
             (tenant_id, workspace_id, owner_principal_id, scope, trigger, title,
              description, body, dialects_json, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10)",
            params![
                tenant.0,
                workspace_id,
                owner_id,
                scope,
                snippet.trigger,
                snippet.title,
                snippet.description,
                snippet.body,
                dialects,
                now,
            ],
        )?;
        snippet = self.sql_snippet_by_id_locked(&conn, SnippetId(conn.last_insert_rowid()))?;
        Ok(snippet)
    }

    pub fn list_sql_snippets_visible(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
        workspace_id: Option<i64>,
    ) -> Result<Vec<SqlSnippet>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, tenant_id, workspace_id, owner_principal_id, scope, trigger,
                    title, description, body, dialects_json, revision
             FROM sql_snippet
             WHERE tenant_id = ?1
               AND (scope = 'tenant' OR owner_principal_id = ?2
                    OR (scope = 'workspace' AND workspace_id = ?3))
             ORDER BY updated_at DESC, id DESC",
        )?;
        let snippets = crate::rows(statement.query_map(
            params![tenant.0, principal.0, workspace_id],
            sql_snippet_from_row,
        )?);
        snippets
    }

    pub fn update_sql_snippet_authorized(
        &self,
        id: SnippetId,
        authorization: SnippetWriteAuthorization,
        expected_revision: u64,
        update: SqlSnippet,
    ) -> Result<SqlSnippet> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT id, tenant_id, workspace_id, owner_principal_id, scope, trigger,
                        title, description, body, dialects_json, revision
                 FROM sql_snippet WHERE id = ?1 AND tenant_id = ?2",
                params![id.0, authorization.tenant.0],
                sql_snippet_from_row,
            )
            .optional()?
            .ok_or(MetadataError::SqlSnippetNotFound(id))?;
        let workspace_editor = existing.scope == SnippetScope::Workspace
            && existing.workspace_id == authorization.editable_workspace;
        if existing.owner_principal_id != Some(authorization.actor.0)
            && !authorization.tenant_admin
            && !workspace_editor
        {
            return Err(MetadataError::SqlSnippetPermissionDenied);
        }
        if existing.revision != expected_revision {
            return Err(MetadataError::SqlSnippetRevisionConflict {
                expected: expected_revision,
                current: existing.revision,
            });
        }
        let dialects = serde_json::to_string(&update.dialects)?;
        tx.execute(
            "UPDATE sql_snippet SET trigger = ?1, title = ?2, description = ?3,
                    body = ?4, dialects_json = ?5, revision = revision + 1,
                    updated_at = ?6 WHERE id = ?7",
            params![
                update.trigger,
                update.title,
                update.description,
                update.body,
                dialects,
                crate::now_text(),
                id.0,
            ],
        )?;
        let updated = self.sql_snippet_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_sql_snippet_authorized(
        &self,
        id: SnippetId,
        authorization: SnippetWriteAuthorization,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = self.sql_snippet_by_id_locked(&tx, id)?;
        let workspace_editor = existing.scope == SnippetScope::Workspace
            && existing.workspace_id == authorization.editable_workspace;
        if existing.tenant_id != Some(authorization.tenant.0)
            || (existing.owner_principal_id != Some(authorization.actor.0)
                && !authorization.tenant_admin
                && !workspace_editor)
        {
            return Err(MetadataError::SqlSnippetPermissionDenied);
        }
        if existing.revision != expected_revision {
            return Err(MetadataError::SqlSnippetRevisionConflict {
                expected: expected_revision,
                current: existing.revision,
            });
        }
        tx.execute("DELETE FROM sql_snippet WHERE id = ?1", params![id.0])?;
        tx.commit()?;
        Ok(())
    }

    fn sql_snippet_by_id_locked(
        &self,
        conn: &rusqlite::Connection,
        id: SnippetId,
    ) -> Result<SqlSnippet> {
        conn.query_row(
            "SELECT id, tenant_id, workspace_id, owner_principal_id, scope, trigger,
                    title, description, body, dialects_json, revision
             FROM sql_snippet WHERE id = ?1",
            params![id.0],
            sql_snippet_from_row,
        )
        .optional()?
        .ok_or(MetadataError::SqlSnippetNotFound(id))
    }
}

fn stored_scope(scope: SnippetScope) -> Result<&'static str> {
    match scope {
        SnippetScope::Personal => Ok("personal"),
        SnippetScope::Workspace => Ok("workspace"),
        SnippetScope::Tenant => Ok("tenant"),
        SnippetScope::BuiltIn | SnippetScope::Catalog => Err(MetadataError::InvalidSqlSnippet(
            "built-in and catalog snippets are immutable".into(),
        )),
    }
}

fn sql_snippet_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SqlSnippet> {
    let scope: String = row.get(4)?;
    let dialects: String = row.get(9)?;
    Ok(SqlSnippet {
        id: Some(SnippetId(row.get(0)?)),
        tenant_id: Some(row.get(1)?),
        workspace_id: row.get(2)?,
        owner_principal_id: row.get(3)?,
        scope: match scope.as_str() {
            "personal" => SnippetScope::Personal,
            "workspace" => SnippetScope::Workspace,
            "tenant" => SnippetScope::Tenant,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        trigger: row.get(5)?,
        title: row.get(6)?,
        description: row.get(7)?,
        body: row.get(8)?,
        dialects: serde_json::from_str::<Vec<DialectId>>(&dialects).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        revision: row.get::<_, i64>(10)?.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sift_protocol::{Engine, SnippetScope};

    use super::*;

    fn snippet() -> SqlSnippet {
        SqlSnippet {
            id: None,
            tenant_id: Some(1),
            workspace_id: None,
            owner_principal_id: Some(1),
            trigger: "selx".into(),
            title: "Select custom".into(),
            description: String::new(),
            body: "SELECT ${1:*} FROM ${2:table};$0".into(),
            dialects: vec![Engine::Postgres.dialect_id()],
            scope: SnippetScope::Personal,
            revision: 0,
        }
    }

    #[test]
    fn snippets_are_visible_and_revision_conflicts_fail() {
        let store =
            MetadataStore::open_in_memory(Arc::new(crate::MemorySecretStore::new())).unwrap();
        store.bootstrap_local("local").unwrap();
        let created = store
            .create_sql_snippet(TenantId(1), PrincipalId(1), snippet())
            .unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(
            store
                .list_sql_snippets_visible(TenantId(1), PrincipalId(1), None)
                .unwrap()
                .len(),
            1
        );
        let error = store
            .update_sql_snippet_authorized(
                created.id.unwrap(),
                SnippetWriteAuthorization {
                    tenant: TenantId(1),
                    actor: PrincipalId(1),
                    tenant_admin: false,
                    editable_workspace: None,
                },
                0,
                snippet(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            MetadataError::SqlSnippetRevisionConflict {
                expected: 0,
                current: 1
            }
        ));
    }
}
