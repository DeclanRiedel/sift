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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextOperation {
    Replace { text: String },
    Insert { offset: usize, text: String },
    Delete { start: usize, end: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("snapshot is not valid utf-8 text: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("operation boundary {boundary} is not a utf-8 character boundary")]
    InvalidBoundary { boundary: usize },
    #[error("delete range start {start} is greater than end {end}")]
    InvalidRange { start: usize, end: usize },
    #[error("operation boundary {boundary} is outside document length {len}")]
    BoundaryOutOfBounds { boundary: usize, len: usize },
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

    pub fn apply(&mut self, operation: TextOperation) -> Result<DocumentSnapshot, DocError> {
        let mut text = self.text()?;
        match operation {
            TextOperation::Replace { text: next } => text = next,
            TextOperation::Insert { offset, text: next } => {
                ensure_boundary(&text, offset)?;
                text.insert_str(offset, &next);
            }
            TextOperation::Delete { start, end } => {
                if start > end {
                    return Err(DocError::InvalidRange { start, end });
                }
                ensure_boundary(&text, start)?;
                ensure_boundary(&text, end)?;
                text.replace_range(start..end, "");
            }
        }
        self.replace_text(text);
        Ok(self.snapshot.clone())
    }

    pub fn snapshot(&self) -> &DocumentSnapshot {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> DocumentSnapshot {
        self.snapshot
    }
}

fn ensure_boundary(text: &str, boundary: usize) -> Result<(), DocError> {
    if boundary > text.len() {
        return Err(DocError::BoundaryOutOfBounds {
            boundary,
            len: text.len(),
        });
    }
    if !text.is_char_boundary(boundary) {
        return Err(DocError::InvalidBoundary { boundary });
    }
    Ok(())
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

    #[test]
    fn text_operations_apply_in_order() {
        let mut doc = TextDocument::from_text(CrdtKind::Loro, "select 1");
        doc.apply(TextOperation::Insert {
            offset: 6,
            text: " *".into(),
        })
        .unwrap();
        assert_eq!(doc.text().unwrap(), "select * 1");

        doc.apply(TextOperation::Delete { start: 8, end: 10 })
            .unwrap();
        assert_eq!(doc.text().unwrap(), "select *");

        doc.apply(TextOperation::Replace {
            text: "select 2".into(),
        })
        .unwrap();
        assert_eq!(doc.text().unwrap(), "select 2");
    }

    #[test]
    fn text_operations_validate_utf8_boundaries() {
        let mut doc = TextDocument::from_text(CrdtKind::Loro, "é");
        assert!(matches!(
            doc.apply(TextOperation::Insert {
                offset: 1,
                text: "!".into()
            }),
            Err(DocError::InvalidBoundary { boundary: 1 })
        ));
        assert!(matches!(
            doc.apply(TextOperation::Delete { start: 2, end: 1 }),
            Err(DocError::InvalidRange { start: 2, end: 1 })
        ));
    }
}
