//! Loro-backed document model for room SQL text.
//!
//! Every client and the server hold a [`TextReplica`]: a Loro CRDT document
//! whose only root container is a single [`loro::LoroText`] named `"text"`.
//! Clients generate native Loro updates; the server validates, sequences, and
//! rebroadcasts them but never authors positional edits on a client's behalf.
//!
//! This crate owns the CRDT primitives — snapshot/update export and import,
//! version-vector and frontier encoding, exact-version materialization, and
//! stable cursors — with no knowledge of persistence, transport, or Tokio. The
//! server drives all Loro CPU work through a per-document blocking actor so it
//! never runs on an async request worker.

use loro::cursor::{Cursor, Side};
use loro::{ExportMode, Frontiers, LoroDoc, LoroValue, VersionVector};

/// The one permitted root container. A valid replica exposes this plain-text
/// container and nothing else.
pub const TEXT_ROOT: &str = "text";

/// Generate a durable, random, non-zero replica peer id from the OS RNG.
///
/// A future durable client persists this alongside the replica snapshot and
/// reuses it; two concurrent writers must never share one document's peer id.
pub fn random_peer_id() -> u64 {
    let mut buf = [0u8; 8];
    getrandom::getrandom(&mut buf).expect("OS RNG is available");
    match u64::from_le_bytes(buf) {
        0 => 1,
        id => id,
    }
}

/// Default ceiling on materialized UTF-8 text, mirrored by the server's
/// `CollaborationConfig::max_document_text_bytes`.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// Default ceiling on a single decoded Loro update, mirrored by the server's
/// `CollaborationConfig::max_document_update_bytes`.
pub const DEFAULT_MAX_UPDATE_BYTES: usize = 1024 * 1024;

/// Errors from CRDT primitives. Transport- and persistence-level errors live in
/// the crates that own those concerns.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("replica peer id must be non-zero")]
    ZeroPeerId,
    #[error("failed to import loro bytes: {0}")]
    Import(String),
    #[error("failed to export loro bytes: {0}")]
    Export(String),
    #[error("failed to decode loro {what}: {detail}")]
    Decode { what: &'static str, detail: String },
    #[error("update exceeds {limit}-byte decoded limit ({actual} bytes)")]
    UpdateTooLarge { actual: usize, limit: usize },
    #[error("materialized text exceeds {limit}-byte limit ({actual} bytes)")]
    TextTooLarge { actual: usize, limit: usize },
    #[error("replica contains a container other than the '{TEXT_ROOT}' text root")]
    UnexpectedContainers,
    #[error("replica contains rich-text marks, which are not allowed")]
    RichTextMark,
    #[error("requested version is not present in retained history")]
    VersionNotFound,
    #[error("byte offset {offset} is not a utf-8 character boundary")]
    NotCharBoundary { offset: usize },
    #[error("range start {start} is greater than end {end}")]
    InvalidRange { start: usize, end: usize },
    #[error("range end {end} is beyond text length {len}")]
    RangeOutOfBounds { end: usize, len: usize },
}

/// Result of importing an update into a replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    /// The update carried at least one new operation that was applied.
    Applied,
    /// The update carried no operation this replica had not already seen.
    NoOp,
    /// The update depends on operations this replica has not yet received; it
    /// was buffered but changed nothing. The caller must resynchronize.
    Pending,
}

/// A Loro replica of a single SQL-text document.
pub struct TextReplica {
    doc: LoroDoc,
}

impl TextReplica {
    /// Create an empty replica that authors ops under `peer_id`.
    ///
    /// `peer_id` must be non-zero and, per the durable-writer invariant, unique
    /// among concurrent writers of one document.
    pub fn new(peer_id: u64) -> Result<Self, DocError> {
        if peer_id == 0 {
            return Err(DocError::ZeroPeerId);
        }
        let doc = LoroDoc::new();
        doc.set_peer_id(peer_id)
            .map_err(|e| DocError::Import(e.to_string()))?;
        // Materialize the text root so an untouched replica still validates as a
        // single-plain-text document.
        let _ = doc.get_text(TEXT_ROOT);
        Ok(Self { doc })
    }

    /// Load a replica from a full Loro snapshot, then adopt `peer_id` for future
    /// local ops. History in the snapshot keeps its original peers.
    pub fn from_snapshot(peer_id: u64, snapshot: &[u8]) -> Result<Self, DocError> {
        if peer_id == 0 {
            return Err(DocError::ZeroPeerId);
        }
        let doc = LoroDoc::new();
        doc.import(snapshot)
            .map_err(|e| DocError::Import(e.to_string()))?;
        doc.set_peer_id(peer_id)
            .map_err(|e| DocError::Import(e.to_string()))?;
        Ok(Self { doc })
    }

    /// The peer id this replica authors ops under.
    pub fn peer_id(&self) -> u64 {
        self.doc.peer_id()
    }

    /// Materialized UTF-8 text at the current version.
    pub fn text(&self) -> String {
        self.doc.get_text(TEXT_ROOT).to_string()
    }

    /// Insert `s` at Unicode-scalar position `pos`, flushing a local commit.
    ///
    /// Positions are Unicode code points, matching Loro's native text index.
    /// Client bindings convert JavaScript UTF-16 offsets before calling.
    pub fn insert(&self, pos: usize, s: &str) -> Result<(), DocError> {
        self.doc
            .get_text(TEXT_ROOT)
            .insert(pos, s)
            .map_err(|e| DocError::Import(e.to_string()))?;
        self.doc.commit();
        Ok(())
    }

    /// Delete `len` Unicode scalars starting at `pos`, flushing a local commit.
    pub fn delete(&self, pos: usize, len: usize) -> Result<(), DocError> {
        self.doc
            .get_text(TEXT_ROOT)
            .delete(pos, len)
            .map_err(|e| DocError::Import(e.to_string()))?;
        self.doc.commit();
        Ok(())
    }

    /// Export a full snapshot: complete history plus current state. Suitable for
    /// durable storage and for bootstrapping a brand-new replica.
    pub fn export_snapshot(&self) -> Result<Vec<u8>, DocError> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|e| DocError::Export(e.to_string()))
    }

    /// Encoded version vector covering everything in this replica's oplog.
    pub fn version_vector(&self) -> Vec<u8> {
        self.doc.oplog_vv().encode()
    }

    /// Encoded frontiers of the current materialized state.
    pub fn frontiers(&self) -> Vec<u8> {
        self.doc.state_frontiers().encode()
    }

    /// Export every operation this replica holds that is absent from the peer
    /// described by the encoded `since` version vector.
    pub fn export_updates_since(&self, since: &[u8]) -> Result<Vec<u8>, DocError> {
        let vv = VersionVector::decode(since).map_err(|e| DocError::Decode {
            what: "version vector",
            detail: e.to_string(),
        })?;
        self.doc
            .export(ExportMode::updates(&vv))
            .map_err(|e| DocError::Export(e.to_string()))
    }

    /// Like [`export_updates_since`](Self::export_updates_since) but returns
    /// `None` when the peer at `since` already has every operation this replica
    /// holds. Loro's raw updates export returns a non-empty envelope even when
    /// there are no new ops, so byte-emptiness cannot distinguish the two; this
    /// compares version vectors instead.
    pub fn updates_since_if_any(&self, since: &[u8]) -> Result<Option<Vec<u8>>, DocError> {
        let vv = VersionVector::decode(since).map_err(|e| DocError::Decode {
            what: "version vector",
            detail: e.to_string(),
        })?;
        if vv.includes_vv(&self.doc.oplog_vv()) {
            return Ok(None);
        }
        let bytes = self
            .doc
            .export(ExportMode::updates(&vv))
            .map_err(|e| DocError::Export(e.to_string()))?;
        Ok(Some(bytes))
    }

    /// Export the replica's entire operation history as an update blob.
    pub fn export_all_updates(&self) -> Result<Vec<u8>, DocError> {
        self.doc
            .export(ExportMode::updates(&VersionVector::default()))
            .map_err(|e| DocError::Export(e.to_string()))
    }

    /// Import an update or snapshot, reporting whether it applied, was a no-op,
    /// or is waiting on missing dependencies. Import is idempotent, so replaying
    /// the same update is safe.
    pub fn import(&self, bytes: &[u8]) -> Result<ImportOutcome, DocError> {
        let status = self
            .doc
            .import(bytes)
            .map_err(|e| DocError::Import(e.to_string()))?;
        if status.pending.as_ref().is_some_and(|p| !p.is_empty()) {
            Ok(ImportOutcome::Pending)
        } else if status.success.is_empty() {
            Ok(ImportOutcome::NoOp)
        } else {
            Ok(ImportOutcome::Applied)
        }
    }

    /// Materialize the exact text at an encoded frontier without mutating this
    /// replica. Errors if the frontier is not in retained history.
    pub fn materialize_at(&self, frontiers: &[u8]) -> Result<String, DocError> {
        Ok(self.fork_at(frontiers)?.text())
    }

    /// Fork an independent replica at the current version. Used to build a
    /// throwaway validation replica so an untrusted update is imported and
    /// checked without touching the committed replica.
    pub fn fork(&self) -> TextReplica {
        TextReplica {
            doc: self.doc.fork(),
        }
    }

    /// Fork an independent replica pinned at an encoded frontier. The fork keeps
    /// this replica's peer id for any subsequent local ops.
    pub fn fork_at(&self, frontiers: &[u8]) -> Result<TextReplica, DocError> {
        let frontiers = Frontiers::decode(frontiers).map_err(|e| DocError::Decode {
            what: "frontiers",
            detail: e.to_string(),
        })?;
        let doc = self
            .doc
            .fork_at(&frontiers)
            .map_err(|_| DocError::VersionNotFound)?;
        Ok(TextReplica { doc })
    }

    /// Encode a stable cursor at Unicode-scalar `pos`. `side` biases the anchor
    /// toward the left or right of concurrent inserts. Returns `None` if the
    /// position is out of range.
    pub fn encode_cursor(&self, pos: usize, side: Side) -> Option<Vec<u8>> {
        self.doc
            .get_text(TEXT_ROOT)
            .get_cursor(pos, side)
            .map(|c| c.encode())
    }

    /// Resolve an encoded cursor to its current Unicode-scalar position after
    /// concurrent edits.
    pub fn resolve_cursor(&self, cursor: &[u8]) -> Result<usize, DocError> {
        let cursor = Cursor::decode(cursor).map_err(|e| DocError::Decode {
            what: "cursor",
            detail: e.to_string(),
        })?;
        let pos = self
            .doc
            .get_cursor_pos(&cursor)
            .map_err(|e| DocError::Decode {
                what: "cursor position",
                detail: e.to_string(),
            })?;
        Ok(pos.current.pos)
    }

    /// Validate that this replica has exactly one plain-text root, carries no
    /// rich-text marks, and stays within `max_text_bytes`. The durable path
    /// imports each incoming update into a throwaway validation replica and
    /// calls this before committing.
    pub fn validate(&self, max_text_bytes: usize) -> Result<(), DocError> {
        match self.doc.get_deep_value() {
            LoroValue::Map(map) => {
                if map.keys().any(|k| k != TEXT_ROOT) {
                    return Err(DocError::UnexpectedContainers);
                }
            }
            _ => return Err(DocError::UnexpectedContainers),
        }
        if let LoroValue::List(items) = self.doc.get_text(TEXT_ROOT).get_richtext_value() {
            for item in items.iter() {
                if let LoroValue::Map(span) = item {
                    if span.get("attributes").is_some() {
                        return Err(DocError::RichTextMark);
                    }
                }
            }
        }
        let bytes = self.doc.get_text(TEXT_ROOT).len_utf8();
        if bytes > max_text_bytes {
            return Err(DocError::TextTooLarge {
                actual: bytes,
                limit: max_text_bytes,
            });
        }
        Ok(())
    }
}

/// Validate and clamp a `[start, end)` byte range against `text`, returning the
/// selected slice. Both boundaries must fall on UTF-8 character boundaries.
///
/// Exact-version execution uses UTF-8 byte offsets into the materialized text;
/// this enforces the boundary and ordering rules the plan requires.
pub fn slice_utf8_range(text: &str, start: usize, end: usize) -> Result<&str, DocError> {
    if start > end {
        return Err(DocError::InvalidRange { start, end });
    }
    if end > text.len() {
        return Err(DocError::RangeOutOfBounds {
            end,
            len: text.len(),
        });
    }
    if !text.is_char_boundary(start) {
        return Err(DocError::NotCharBoundary { offset: start });
    }
    if !text.is_char_boundary(end) {
        return Err(DocError::NotCharBoundary { offset: end });
    }
    Ok(&text[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER_A: u64 = 0xA1;
    const PEER_B: u64 = 0xB2;
    const PEER_C: u64 = 0xC3;

    fn merge(dst: &TextReplica, src: &TextReplica) {
        let update = src.export_all_updates().unwrap();
        dst.import(&update).unwrap();
    }

    #[test]
    fn rejects_zero_peer_id() {
        assert!(matches!(TextReplica::new(0), Err(DocError::ZeroPeerId)));
    }

    #[test]
    fn empty_replica_validates_and_round_trips_snapshot() {
        let a = TextReplica::new(PEER_A).unwrap();
        assert_eq!(a.text(), "");
        a.validate(DEFAULT_MAX_TEXT_BYTES).unwrap();

        a.insert(0, "select 1").unwrap();
        let snapshot = a.export_snapshot().unwrap();
        let b = TextReplica::from_snapshot(PEER_B, &snapshot).unwrap();
        assert_eq!(b.text(), "select 1");
        assert_eq!(b.peer_id(), PEER_B);
    }

    #[test]
    fn concurrent_inserts_at_same_position_converge() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "SELECT ").unwrap();
        let base = a.export_snapshot().unwrap();
        let b = TextReplica::from_snapshot(PEER_B, &base).unwrap();

        a.insert(7, "a").unwrap();
        b.insert(7, "b").unwrap();

        // Exchange updates in both directions.
        merge(&a, &b);
        merge(&b, &a);

        assert_eq!(a.text(), b.text());
        // Both concurrent characters survive; order is deterministic.
        assert!(a.text().starts_with("SELECT "));
        assert_eq!(a.text().len(), "SELECT ab".len());
    }

    #[test]
    fn overlapping_insert_and_delete_converge() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "hello world").unwrap();
        let base = a.export_snapshot().unwrap();
        let b = TextReplica::from_snapshot(PEER_B, &base).unwrap();

        a.delete(0, 6).unwrap(); // remove "hello "
        b.insert(11, "!").unwrap(); // append "!"

        merge(&a, &b);
        merge(&b, &a);
        assert_eq!(a.text(), b.text());
        assert_eq!(a.text(), "world!");
    }

    #[test]
    fn multibyte_unicode_edits_converge() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "café ☕").unwrap();
        let base = a.export_snapshot().unwrap();
        let b = TextReplica::from_snapshot(PEER_B, &base).unwrap();

        // Positions are Unicode scalars: "café ☕" is 6 code points.
        a.insert(6, " noir").unwrap();
        b.insert(0, "un ").unwrap();

        merge(&a, &b);
        merge(&b, &a);
        assert_eq!(a.text(), b.text());
        assert_eq!(a.text(), "un café ☕ noir");
    }

    #[test]
    fn out_of_order_duplicate_and_reordered_updates_converge() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "a").unwrap();
        let u1 = a.export_all_updates().unwrap();
        a.insert(1, "b").unwrap();
        let vv1 = {
            let tmp = TextReplica::from_snapshot(PEER_C, &u1).unwrap();
            tmp.version_vector()
        };
        let u2 = a.export_updates_since(&vv1).unwrap();

        // Apply out of order (u2 before u1) and duplicated. u2 buffers until u1
        // arrives, at which point Loro applies both; further replays are no-ops.
        let b = TextReplica::new(PEER_B).unwrap();
        assert_eq!(b.import(&u2).unwrap(), ImportOutcome::Pending);
        assert_eq!(b.import(&u1).unwrap(), ImportOutcome::Applied);
        assert_eq!(b.text(), "ab");
        assert_eq!(b.import(&u2).unwrap(), ImportOutcome::NoOp);
        assert_eq!(b.import(&u1).unwrap(), ImportOutcome::NoOp);
    }

    #[test]
    fn snapshot_merges_with_arbitrarily_old_replica() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "one").unwrap();
        // b forks from an old snapshot and diverges offline.
        let old = a.export_snapshot().unwrap();
        let b = TextReplica::from_snapshot(PEER_B, &old).unwrap();
        for _ in 0..300 {
            let len = a.text().chars().count();
            a.insert(len, "x").unwrap();
        }
        b.insert(3, " two").unwrap();

        // Old replica merges the whole new snapshot; server merges b's updates.
        let snap = a.export_snapshot().unwrap();
        b.import(&snap).unwrap();
        merge(&a, &b);
        // Both concurrent edit streams survive and converge byte-for-byte. The
        // 300 appends and " two" share the anchor after "one", so their
        // interleaving is deterministic but not "one two" first.
        assert_eq!(a.text(), b.text());
        assert!(a.text().starts_with("one"));
        assert!(a.text().contains("two"));
        assert_eq!(a.text().matches('x').count(), 300);
    }

    #[test]
    fn exact_historical_frontier_materializes_expected_text() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "one").unwrap();
        let frontier = a.frontiers();
        let text_at = a.materialize_at(&frontier).unwrap();
        assert_eq!(text_at, "one");

        a.insert(3, " two").unwrap();
        assert_eq!(a.text(), "one two");
        // The old frontier still materializes the old text.
        assert_eq!(a.materialize_at(&frontier).unwrap(), "one");
    }

    #[test]
    fn stable_cursor_survives_concurrent_edits() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "SELECT * FROM t").unwrap();
        // Anchor at the '*' (position 7).
        let cursor = a.encode_cursor(7, Side::Left).unwrap();
        assert_eq!(a.resolve_cursor(&cursor).unwrap(), 7);

        a.insert(0, "-- c\n").unwrap();
        // The anchor shifts right by the inserted prefix length.
        assert_eq!(a.resolve_cursor(&cursor).unwrap(), 7 + 5);
    }

    #[test]
    fn extra_root_container_is_rejected() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "ok").unwrap();
        // Author a second root container directly on the underlying doc.
        {
            let doc = &a.doc;
            doc.get_map("rogue").insert("k", "v").unwrap();
            doc.commit();
        }
        assert!(matches!(
            a.validate(DEFAULT_MAX_TEXT_BYTES),
            Err(DocError::UnexpectedContainers)
        ));
    }

    #[test]
    fn rich_text_mark_is_rejected() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "bold").unwrap();
        {
            let text = a.doc.get_text(TEXT_ROOT);
            text.mark(0..4, "bold", true).unwrap();
            a.doc.commit();
        }
        assert!(matches!(
            a.validate(DEFAULT_MAX_TEXT_BYTES),
            Err(DocError::RichTextMark)
        ));
    }

    #[test]
    fn text_size_limit_is_enforced() {
        let a = TextReplica::new(PEER_A).unwrap();
        a.insert(0, "abcdef").unwrap();
        assert!(matches!(
            a.validate(4),
            Err(DocError::TextTooLarge {
                actual: 6,
                limit: 4
            })
        ));
        a.validate(6).unwrap();
    }

    #[test]
    fn utf8_range_slicing_enforces_boundaries() {
        let text = "café";
        assert_eq!(slice_utf8_range(text, 0, 3).unwrap(), "caf");
        // 'é' occupies bytes 3..5; slicing at byte 4 is mid-character.
        assert!(matches!(
            slice_utf8_range(text, 0, 4),
            Err(DocError::NotCharBoundary { offset: 4 })
        ));
        assert!(matches!(
            slice_utf8_range(text, 3, 1),
            Err(DocError::InvalidRange { start: 3, end: 1 })
        ));
        assert!(matches!(
            slice_utf8_range(text, 0, 99),
            Err(DocError::RangeOutOfBounds { .. })
        ));
    }
}
