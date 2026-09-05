//! Bounded chunk assembly for one document's sync lane.

use sift_doc::DocError;
use sift_protocol::{DocumentTransferKind, DocumentVersion};
use std::collections::HashMap;

// The server retains at most 256 MiB of CRDT history by default and emits
// 256 KiB chunks. Bound slot allocation separately from retained payload bytes.
const MAX_TRANSFER_BYTES: usize = 256 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_CHUNKS: u32 = 4096;
const MAX_TRANSFERS: usize = 4;

struct Transfer {
    request_id: String,
    kind: DocumentTransferKind,
    version: DocumentVersion,
    snapshot_seq: i64,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Transfers(HashMap<String, Transfer>);

fn invalid(detail: &'static str) -> DocError {
    DocError::Decode {
        what: "document transfer",
        detail: detail.into(),
    }
}

impl Transfers {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn ingest(
        &mut self,
        message: &sift_protocol::RoomServerMessage,
    ) -> Result<Option<Vec<u8>>, DocError> {
        let result = self.ingest_chunk(message);
        if result.is_err() {
            self.clear();
        }
        result
    }

    fn ingest_chunk(
        &mut self,
        message: &sift_protocol::RoomServerMessage,
    ) -> Result<Option<Vec<u8>>, DocError> {
        let sift_protocol::RoomServerMessage::DocumentChunk {
            request_id,
            transfer_id,
            index,
            count,
            payload,
            payload_kind,
            snapshot_seq,
            server_version,
            ..
        } = message
        else {
            unreachable!("only document chunks are assembled")
        };
        if *count == 0 || *count > MAX_CHUNKS || *index >= *count {
            return Err(invalid("invalid chunk count or index"));
        }
        let payload = payload.as_bytes();
        if payload.len() > MAX_CHUNK_BYTES {
            return Err(invalid("chunk exceeds decoded size limit"));
        }
        let retained = self
            .0
            .values()
            .map(|transfer| transfer.bytes)
            .sum::<usize>();
        if !self.0.contains_key(transfer_id) && self.0.len() >= MAX_TRANSFERS {
            return Err(invalid("too many incomplete transfers"));
        }
        let transfer = self
            .0
            .entry(transfer_id.clone())
            .or_insert_with(|| Transfer {
                request_id: request_id.clone(),
                kind: *payload_kind,
                version: server_version.clone(),
                snapshot_seq: *snapshot_seq,
                chunks: vec![None; *count as usize],
                received: 0,
                bytes: 0,
            });
        if transfer.chunks.len() != *count as usize
            || transfer.request_id != *request_id
            || transfer.kind != *payload_kind
            || transfer.version != *server_version
            || transfer.snapshot_seq != *snapshot_seq
        {
            return Err(invalid("chunk metadata changed within a transfer"));
        }
        let slot = &mut transfer.chunks[*index as usize];
        if let Some(previous) = slot {
            if previous != payload {
                return Err(invalid("duplicate chunk has different contents"));
            }
            return Ok(None);
        }
        if payload.len() > MAX_TRANSFER_BYTES.saturating_sub(retained) {
            return Err(invalid("incomplete transfers exceed decoded size limit"));
        }
        *slot = Some(payload.to_vec());
        transfer.bytes += payload.len();
        transfer.received += 1;
        if transfer.received != transfer.chunks.len() {
            return Ok(None);
        }
        let transfer = self.0.remove(transfer_id).unwrap();
        let mut bytes = Vec::with_capacity(transfer.bytes);
        for chunk in transfer.chunks.into_iter().flatten() {
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{CrdtUpdate, RoomServerMessage};

    fn chunk(id: &str, index: u32, count: u32, bytes: Vec<u8>) -> RoomServerMessage {
        RoomServerMessage::DocumentChunk {
            request_id: "request".into(),
            document_id: 1,
            transfer_id: id.into(),
            index,
            count,
            payload: CrdtUpdate::new(bytes),
            payload_kind: DocumentTransferKind::Snapshot,
            snapshot_seq: 0,
            server_version: DocumentVersion::new(Vec::new()),
        }
    }

    #[test]
    fn hostile_or_inconsistent_chunks_fail_without_retaining_transfers() {
        let mut transfers = Transfers::default();
        for (index, count) in [(0, 0), (0, u32::MAX), (2, 2)] {
            assert!(transfers.ingest(&chunk("t", index, count, vec![])).is_err());
            assert!(transfers.is_empty());
        }
        assert!(transfers
            .ingest(&chunk("t", 0, 2, vec![0; MAX_CHUNK_BYTES + 1]))
            .is_err());
        for bad in [chunk("t", 1, 3, vec![]), chunk("t", 0, 2, vec![2])] {
            transfers.ingest(&chunk("t", 0, 2, vec![1])).unwrap();
            assert!(transfers.ingest(&bad).is_err());
            assert!(transfers.is_empty());
        }
    }

    #[test]
    fn incomplete_transfer_admission_is_bounded() {
        let mut transfers = Transfers::default();
        for index in 0..MAX_TRANSFERS {
            assert!(transfers
                .ingest(&chunk(&index.to_string(), 0, 2, vec![1]))
                .unwrap()
                .is_none());
        }
        assert!(transfers.ingest(&chunk("extra", 0, 2, vec![1])).is_err());
        assert!(transfers.is_empty());
    }
}
