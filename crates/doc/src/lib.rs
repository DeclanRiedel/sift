//! Minimal document abstraction for room documents.
//!
//! Metadata owns persistence and stores opaque snapshot bytes. This crate owns
//! the application-facing document contract so future Loro/Automerge plumbing
//! can land behind one API.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrdtKind {
    Loro,
    Automerge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSnapshot {
    pub crdt: CrdtKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDocument {
    snapshot: DocumentSnapshot,
}

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("snapshot is not valid utf-8 text: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

impl DocumentSnapshot {
    pub fn new(crdt: CrdtKind, bytes: Vec<u8>) -> Self {
        Self { crdt, bytes }
    }

    pub fn empty_text(crdt: CrdtKind) -> Self {
        Self {
            crdt,
            bytes: Vec::new(),
        }
    }
}

impl TextDocument {
    pub fn from_text(crdt: CrdtKind, text: impl Into<String>) -> Self {
        Self {
            snapshot: DocumentSnapshot::new(crdt, text.into().into_bytes()),
        }
    }

    pub fn from_snapshot(snapshot: DocumentSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn text(&self) -> Result<String, DocError> {
        String::from_utf8(self.snapshot.bytes.clone()).map_err(Into::into)
    }

    pub fn replace_text(&mut self, text: impl Into<String>) {
        self.snapshot.bytes = text.into().into_bytes();
    }

    pub fn snapshot(&self) -> &DocumentSnapshot {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> DocumentSnapshot {
        self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_document_round_trips_snapshot_bytes() {
        let doc = TextDocument::from_text(CrdtKind::Loro, "select 1");
        let snapshot = doc.into_snapshot();
        assert_eq!(snapshot.crdt, CrdtKind::Loro);
        assert_eq!(snapshot.bytes, b"select 1");

        let doc = TextDocument::from_snapshot(snapshot);
        assert_eq!(doc.text().unwrap(), "select 1");
    }

    #[test]
    fn text_document_replace_updates_snapshot() {
        let mut doc = TextDocument::from_text(CrdtKind::Automerge, "old");
        doc.replace_text("new");
        assert_eq!(doc.text().unwrap(), "new");
        assert_eq!(doc.snapshot().bytes, b"new");
    }
}
