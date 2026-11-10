-- Reconciliation acknowledges uncertainty; it never rewrites the native receipt.
ALTER TABLE ai_proposal_apply ADD COLUMN reconciled_at TEXT;
ALTER TABLE ai_proposal_apply ADD COLUMN reconciled_by INTEGER REFERENCES principal(id)
    ON DELETE RESTRICT CHECK((reconciled_at IS NULL) = (reconciled_by IS NULL));

CREATE TRIGGER ai_apply_shared_admission BEFORE INSERT ON ai_proposal_apply
WHEN NEW.state='applying' AND (
    EXISTS(SELECT 1 FROM ai_sql_execution s JOIN ai_proposal p ON p.chat_id=s.chat_id
        WHERE p.id=NEW.proposal_id AND s.state IN ('applying','outcome_unknown'))
    OR EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id
        JOIN ai_proposal n ON n.chat_id=p.chat_id WHERE n.id=NEW.proposal_id
        AND (a.state='applying' OR (a.state='outcome_unknown' AND a.reconciled_at IS NULL)))
)
BEGIN SELECT RAISE(ABORT,'AI mutation must settle or be acknowledged before another action'); END;

CREATE TRIGGER ai_sql_shared_admission BEFORE UPDATE OF state ON ai_sql_execution
WHEN NEW.state='applying' AND (
    EXISTS(SELECT 1 FROM ai_sql_execution s WHERE s.chat_id=NEW.chat_id AND s.id<>NEW.id
        AND s.state IN ('applying','outcome_unknown'))
    OR EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id
        WHERE p.chat_id=NEW.chat_id
        AND (a.state='applying' OR (a.state='outcome_unknown' AND a.reconciled_at IS NULL)))
)
BEGIN SELECT RAISE(ABORT,'AI mutation must settle or be acknowledged before another action'); END;

CREATE TRIGGER ai_apply_keep_active_chat BEFORE DELETE ON ai_chat
WHEN EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id
    WHERE p.chat_id=OLD.id AND a.state='applying')
BEGIN SELECT RAISE(ABORT,'AI database application still has native work pending'); END;
