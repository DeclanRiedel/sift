//! Request correlation IDs (Phase B reliability step 5).
//!
//! Every HTTP request and WebSocket connection carries a correlation ID —
//! taken from the inbound `x-correlation-id` header or generated when absent.
//! It is echoed in the response header, attached to the request's tracing
//! span, and recorded on durable audit rows, so one identifier ties a client
//! request to its server-side logs and audit trail.
//!
//! The ID rides a task-local rather than being threaded through every handler
//! and audit call site: middleware runs the request inside [`scope`], and any
//! code on that task reads it via [`current`].

use axum::http::header::HeaderName;

/// Wire header carrying the correlation ID, in and out.
pub const CORRELATION_HEADER: HeaderName = HeaderName::from_static("x-correlation-id");

tokio::task_local! {
    static CORRELATION_ID: String;
}

/// Run `future` with `id` installed as the current correlation ID.
pub async fn scope<F>(id: String, future: F) -> F::Output
where
    F: std::future::Future,
{
    CORRELATION_ID.scope(id, future).await
}

/// The correlation ID for the current task, if one is in scope.
pub fn current() -> Option<String> {
    CORRELATION_ID.try_with(|id| id.clone()).ok()
}

/// Generate a fresh correlation ID.
pub fn generate() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Sanitize a client-supplied correlation ID: keep it only if it is a
/// reasonable length and printable ASCII, otherwise generate a fresh one. This
/// stops a client from smuggling control characters or unbounded data into log
/// lines and audit rows via the header.
pub fn sanitize(candidate: &str) -> Option<String> {
    let trimmed = candidate.trim();
    let valid = (1..=128).contains(&trimmed.len())
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_graphic() || b == b'-' || b == b'_');
    valid.then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_reads_scoped_id() {
        assert_eq!(current(), None);
        let seen = scope("abc".to_string(), async { current() }).await;
        assert_eq!(seen.as_deref(), Some("abc"));
        assert_eq!(current(), None);
    }

    #[test]
    fn sanitize_rejects_junk_and_keeps_clean_ids() {
        assert_eq!(sanitize("req-123_ABC").as_deref(), Some("req-123_ABC"));
        assert_eq!(sanitize("  spaced  ").as_deref(), Some("spaced"));
        assert_eq!(sanitize(""), None);
        assert_eq!(sanitize("has space"), None);
        assert_eq!(sanitize("bad\nnewline"), None);
        assert_eq!(sanitize(&"x".repeat(200)), None);
    }
}
