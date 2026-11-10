//! Atomic chat admission shared by native SQL and reviewed database applies.
use super::{MetadataError, Result};
use rusqlite::Connection;

pub(super) fn require_clear(conn: &Connection, chat: &str) -> Result<()> {
    let busy: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ai_sql_execution WHERE chat_id=?1 AND state IN ('applying','outcome_unknown'))
         OR EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id
             WHERE p.chat_id=?1 AND (a.state='applying' OR (a.state='outcome_unknown' AND a.reconciled_at IS NULL)))",
        [chat], |row| row.get(0),
    )?;
    if busy {
        return Err(MetadataError::AiInvalid(
            "A native or uncertain mutation must settle or be acknowledged before another action"
                .into(),
        ));
    }
    Ok(())
}
