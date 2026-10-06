-- Cancellation proof is an opaque request identity, not a provider credential.
CREATE TABLE ai_pending_turn_cancel (
    chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
    client_request_id TEXT NOT NULL,
    desktop_id TEXT NOT NULL,
    initiator_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (chat_id, client_request_id, desktop_id, initiator_principal_id)
);
