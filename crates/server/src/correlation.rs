//! Request IDs correlate responses, tracing and durable audits. Middleware
//! installs task-local scope; detached work must capture and restore it.

use axum::http::header::HeaderName;

pub const CORRELATION_HEADER: HeaderName = HeaderName::from_static("x-correlation-id");

tokio::task_local! {
    static CORRELATION_ID: String;
}

pub async fn scope<F>(id: String, future: F) -> F::Output
where
    F: std::future::Future,
{
    CORRELATION_ID.scope(id, future).await
}

/// Restore captured request identity for synchronous audit/drop settlement.
pub(crate) fn sync_scope<F: FnOnce() -> T, T>(id: String, work: F) -> T {
    CORRELATION_ID.sync_scope(id, work)
}

pub fn current() -> Option<String> {
    CORRELATION_ID.try_with(|id| id.clone()).ok()
}

pub fn generate() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Accept bounded printable ASCII only, preventing control-character and
/// oversized-data injection into logs and audits.
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
