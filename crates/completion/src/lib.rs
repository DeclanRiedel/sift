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

mod dictionary;
pub mod fuzzy;
mod keywords;
mod rank;

pub use dictionary::Dictionary;
pub use fuzzy::{fuzzy_match, FuzzyMatch};
pub use sift_semantic::CompletionAnalysis as ContextResult;

/// Compatibility entry point. Stateful callers obtain this analysis from the
/// revisioned semantic document instead of supplying SQL again.
pub fn detect_context(sql: &str, cursor: usize, engine: Engine) -> ContextResult {
    let cursor = floor_char_boundary(sql, usize::min(cursor, sql.len()));
    sift_semantic::detect_completion_context(sql, cursor, &engine.dialect_id())
        .expect("built-in dialect and clamped cursor are valid")
}

/// Server-facing entry point: compute ranked completion candidates for
/// `req.sql` at byte offset `req.cursor`, using `snapshot` as the source
/// of truth for schema names.
pub fn complete(
    req: &CompletionRequest,
    snapshot: &SchemaSnapshot,
    engine: Engine,
) -> CompletionResponse {
    let dict = dictionary::Dictionary::from_snapshot(snapshot);
    complete_with_dictionary(req, &dict, engine)
}

/// Compute ranked completions using a prebuilt dictionary. Server hot paths
/// use this when the schema cache already owns the dictionary for a snapshot.
pub fn complete_with_dictionary(
    req: &CompletionRequest,
    dict: &Dictionary,
    engine: Engine,
) -> CompletionResponse {
    let cursor = usize::min(req.cursor as usize, req.sql.len());
    let ctx = detect_context(&req.sql, cursor, engine);
    complete_with_context(req, &ctx, dict, engine)
}

/// Rank completions from a context that has already been detected. This keeps
/// server orchestration from recomputing SQL context when it upgrades a shallow
/// completion with deep schema data.
pub fn complete_with_context(
    req: &CompletionRequest,
    ctx: &ContextResult,
    dict: &Dictionary,
    engine: Engine,
) -> CompletionResponse {
    complete_with_analysis(req.limit, ctx, dict, engine)
}

/// Rank completion from shared semantic analysis without carrying SQL text.
pub fn complete_with_analysis(
    limit: Option<u32>,
    ctx: &ContextResult,
    dict: &Dictionary,
    engine: Engine,
) -> CompletionResponse {
    let limit = limit.map(|l| usize::min(l as usize, 200)).unwrap_or(50);
    let candidates = rank::rank(ctx, dict, engine, limit);
    CompletionResponse {
        candidates,
        replaced_range: sift_protocol::completion::Range {
            start: ctx.prefix_start as u32,
            end: ctx.cursor as u32,
        },
        context: ctx.context.clone(),
    }
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}
