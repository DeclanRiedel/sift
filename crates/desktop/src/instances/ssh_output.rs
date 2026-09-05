//! Bounded SSH helper output and cancellation-owned diagnostic draining.

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _};

const MAX_READY_BYTES: usize = 64 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

pub(super) async fn read_ready(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<sift_protocol::RemoteReady>, String> {
    let mut bytes = Vec::new();
    let count = reader
        .take((MAX_READY_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| "reading SSH helper readiness failed".to_owned())?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_READY_BYTES {
        return Err("SSH helper readiness exceeded 64 KiB".into());
    }
    // Serde errors may include input values. Readiness carries an access token,
    // so neither the original line nor decoder details belong in UI errors.
    let ready: sift_protocol::RemoteReady = serde_json::from_slice(&bytes)
        .map_err(|_| "SSH helper returned invalid readiness JSON".to_owned())?;
    let url = reqwest::Url::parse(&ready.local_base_url)
        .map_err(|_| "SSH helper returned an invalid forwarding URL".to_owned())?;
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err("SSH helper forwarding URL must be an HTTP loopback origin".into());
    }
    Ok(Some(ready))
}

pub(super) struct Diagnostics {
    bytes: Arc<Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Diagnostics {
    pub(super) fn drain(mut reader: impl AsyncRead + Unpin + Send + 'static) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let output = bytes.clone();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            while let Ok(count) = reader.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                let mut output = output.lock().unwrap();
                let retained = count.min((MAX_DIAGNOSTIC_BYTES + 1).saturating_sub(output.len()));
                output.extend_from_slice(&buffer[..retained]);
            }
        });
        Self { bytes, task }
    }

    pub(super) async fn failure(&mut self, summary: &str) -> String {
        // Descendants can inherit a pipe after the helper exits. Never wait
        // indefinitely for their EOF while reporting a failed connection.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.task).await;
        let bytes = self.bytes.lock().unwrap();
        let truncated = bytes.len() > MAX_DIAGNOSTIC_BYTES;
        let detail = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DIAGNOSTIC_BYTES)]);
        let detail = detail.trim();
        if detail.is_empty() {
            summary.to_owned()
        } else if truncated {
            format!("{summary}: {detail}… [diagnostics truncated]")
        } else {
            format!("{summary}: {detail}")
        }
    }
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt as _;

    fn ready(url: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "local_base_url": url,
            "access_token": "token-must-not-appear-in-errors",
            "access_expires_at": "2026-09-06T00:00:00Z",
            "principal_id": 1,
            "instance_id": "instance",
            "daemon_generation": "generation",
            "server_version": "0.1.0",
            "selected_protocol": 1
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn readiness_is_bounded_redacted_and_loopback_only() {
        for url in ["http://127.0.0.1:7474", "http://[::1]:7474"] {
            assert!(read_ready(&mut ready(url).as_slice())
                .await
                .unwrap()
                .is_some());
        }
        for url in [
            "https://example.test",
            "http://0.0.0.0:7474",
            "http://user:secret@127.0.0.1:7474",
            "http://127.0.0.1:7474/path",
        ] {
            assert!(read_ready(&mut ready(url).as_slice()).await.is_err());
        }
        let malformed = b"{\"access_token\":\"token-must-not-appear-in-errors\",";
        let error = read_ready(&mut malformed.as_slice()).await.unwrap_err();
        assert!(!error.contains("token-must-not-appear-in-errors"));
        let oversized = vec![b'x'; MAX_READY_BYTES + 1];
        assert!(read_ready(&mut oversized.as_slice())
            .await
            .unwrap_err()
            .contains("64 KiB"));
    }

    #[tokio::test]
    async fn diagnostics_drain_beyond_retention_without_blocking_the_writer() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut diagnostics = Diagnostics::drain(reader);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            writer
                .write_all(&vec![b'x'; MAX_DIAGNOSTIC_BYTES * 3])
                .await
                .unwrap();
            writer.shutdown().await.unwrap();
        })
        .await
        .unwrap();
        let error = diagnostics.failure("bootstrap failed").await;
        assert!(error.ends_with("[diagnostics truncated]"));
        assert_eq!(
            diagnostics.bytes.lock().unwrap().len(),
            MAX_DIAGNOSTIC_BYTES + 1
        );
    }
}
