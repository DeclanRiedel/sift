//! `sift-completion` — SQL autocomplete engine.
//!
//! Pure Rust; no I/O, no tokio. Consumed by the server (via the HTTP
//! autocomplete route) and by any future client that wants to compute
//! completions locally from a cached `SchemaSnapshot`.
//!
//! The public entry point is [`complete`]. Given a request (SQL + cursor
//! byte offset), a schema snapshot, and the engine, it returns a
//! [`CompletionResponse`] with ranked candidates.
//!
//! Design notes are in `docs/PLANS/server-build-list-v2.md` (Phase D) and
//! parallel the existing `crates/server/src/ddl.rs` server-side
//! composition pattern — no new `Driver` trait method (ADR-017).

use sift_protocol::completion::{CompletionRequest, CompletionResponse};
use sift_protocol::{Engine, SchemaSnapshot};

mod context;
mod dictionary;
mod keywords;
mod rank;

pub use context::{detect_context, ContextResult};
pub use dictionary::Dictionary;

/// Server-facing entry point: compute ranked completion candidates for
/// `req.sql` at byte offset `req.cursor`, using `snapshot` as the source
/// of truth for schema names.
pub fn complete(
    req: &CompletionRequest,
    snapshot: &SchemaSnapshot,
    engine: Engine,
) -> CompletionResponse {
    let cursor = usize::min(req.cursor as usize, req.sql.len());
    let ctx = context::detect_context(&req.sql, cursor, engine);
    let dict = dictionary::Dictionary::from_snapshot(snapshot);
    let limit = req.limit.map(|l| usize::min(l as usize, 200)).unwrap_or(50);
    let candidates = rank::rank(&ctx, &dict, engine, limit);
    CompletionResponse {
        candidates,
        replaced_range: sift_protocol::completion::Range {
            start: ctx.prefix_start as u32,
            end: ctx.cursor as u32,
        },
        context: ctx.context,
    }
}
