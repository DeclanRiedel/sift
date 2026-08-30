use super::*;

impl MetadataStore {
    pub fn issue_api_token(
        &self,
        principal: PrincipalId,
        scope: Option<TenantId>,
        name: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(ApiTokenRow, String)> {
        let lookup_seed = Uuid::new_v4().simple().to_string();
        let token_lookup = &lookup_seed[..API_TOKEN_LOOKUP_LEN];
        let plaintext = format!(
            "{API_TOKEN_PREFIX}{token_lookup}_{}",
            Uuid::new_v4().simple()
        );
        let token_mac = token_mac(&plaintext);
        let salt = SaltString::generate(&mut OsRng);
        let token_hash = Argon2::default()
            .hash_password(plaintext.as_bytes(), &salt)
            .map_err(password_hash_error)?
            .to_string();
        let now = now_text();
        let expires_at_text = expires_at.map(|dt| dt.to_rfc3339());
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO api_token
             (principal_id, tenant_id, token_lookup, token_hash, token_mac, name, created_at, updated_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
            params![
                principal.0,
                scope.map(|id| id.0),
                token_lookup,
                token_hash,
                token_mac,
                name,
                now,
                expires_at_text
            ],
        )?;
        let id = ApiTokenId(conn.last_insert_rowid());
        let row = self.api_token_by_id_locked(&conn, id)?;
        Ok((row, plaintext))
    }

    pub fn verify_api_token(&self, presented: &str) -> Result<Option<ApiTokenRow>> {
        let Some(token_lookup) = token_lookup_from_presented(presented) else {
            return Ok(None);
        };
        let now = Utc::now();
        let conn = self.conn()?;
        let candidate = conn
            .query_row(
                "SELECT id, token_hash, token_mac, last_used_at FROM api_token
                 WHERE token_lookup = ?1
                   AND revoked_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?2)",
                params![token_lookup, now.to_rfc3339()],
                |row| {
                    Ok((
                        ApiTokenId(row.get(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, hash, mac, last_used_at)) = candidate else {
            return Ok(None);
        };

        let verified = mac
            .as_deref()
            .is_some_and(|mac| constant_time_eq(mac.as_bytes(), token_mac(presented).as_bytes()))
            || verify_legacy_token(presented, &hash)?;
        if !verified {
            return Ok(None);
        }

        if should_touch_token(last_used_at.as_deref(), now) {
            let used_at = now.to_rfc3339();
            conn.execute(
                "UPDATE api_token SET last_used_at = ?1, updated_at = ?1 WHERE id = ?2",
                params![used_at, id.0],
            )?;
        }
        self.api_token_by_id_locked(&conn, id).map(Some)
    }

    pub fn list_api_tokens(&self, principal: PrincipalId) -> Result<Vec<ApiTokenRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, principal_id, tenant_id, name, created_at, updated_at,
                    last_used_at, expires_at, revoked_at
             FROM api_token
             WHERE principal_id = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let tokens = rows(stmt.query_map(params![principal.0], api_token_from_row)?);
        tokens
    }

    /// Revoke an API token and write its audit row in the same transaction.
    pub fn revoke_api_token(&self, id: ApiTokenId, audit: NewOperationAudit) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE api_token
             SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1
             WHERE id = ?2",
            params![now, id.0],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }
}
