//! Fixed-cardinality HTTP metrics and opt-in OTLP/HTTP JSON request spans.
//! Deliberately excludes request URLs, headers, SQL, bodies and response data.
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::{
    fmt::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static EXPORTER: OnceLock<tokio::sync::mpsc::Sender<serde_json::Value>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);
const BUCKETS: [u64; 7] = [1_000, 5_000, 10_000, 50_000, 100_000, 1_000_000, 10_000_000];

#[derive(Default)]
pub(crate) struct Metrics {
    completed: [AtomicU64; 6],
    inflight: AtomicU64,
    duration_us: AtomicU64,
    buckets: [AtomicU64; 7],
}

impl Metrics {
    fn record(&self, status: u16, elapsed: Duration) {
        self.completed[(status as usize / 100).min(5)].fetch_add(1, Ordering::Relaxed);
        let micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.duration_us.fetch_add(micros, Ordering::Relaxed);
        for (bound, count) in BUCKETS.iter().zip(&self.buckets) {
            if micros <= *bound {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn render(&self) -> String {
        let mut text = String::from("# HELP sift_http_requests_total Completed HTTP requests by status class.\n# TYPE sift_http_requests_total counter\n");
        let mut total = 0;
        for (class, counter) in self.completed.iter().enumerate() {
            let count = counter.load(Ordering::Relaxed);
            total += count;
            writeln!(
                text,
                "sift_http_requests_total{{status_class=\"{class}xx\"}} {count}"
            )
            .unwrap();
        }
        writeln!(
            text,
            "# TYPE sift_http_requests_in_flight gauge\nsift_http_requests_in_flight {}",
            self.inflight.load(Ordering::Relaxed)
        )
        .unwrap();
        text.push_str("# TYPE sift_http_request_duration_seconds histogram\n");
        for (bound, count) in BUCKETS.iter().zip(&self.buckets) {
            writeln!(
                text,
                "sift_http_request_duration_seconds_bucket{{le=\"{}\"}} {}",
                *bound as f64 / 1e6,
                count.load(Ordering::Relaxed)
            )
            .unwrap();
        }
        writeln!(text, "sift_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {total}\nsift_http_request_duration_seconds_count {total}\nsift_http_request_duration_seconds_sum {}", self.duration_us.load(Ordering::Relaxed) as f64 / 1e6).unwrap();
        writeln!(
            text,
            "# TYPE sift_otel_spans_dropped_total counter\nsift_otel_spans_dropped_total {}",
            DROPPED.load(Ordering::Relaxed)
        )
        .unwrap();
        text
    }
}

struct Inflight(Arc<Metrics>);
impl Drop for Inflight {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) async fn observe(
    State(metrics): State<Arc<Metrics>>,
    request: Request,
    next: Next,
) -> Response {
    metrics.inflight.fetch_add(1, Ordering::Relaxed);
    let _guard = Inflight(metrics.clone());
    let method = match request.method().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    let started = Instant::now();
    let unix_start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    metrics.record(response.status().as_u16(), elapsed);
    if let Some(exporter) = EXPORTER.get() {
        if exporter
            .try_send(span(
                method,
                response.status().as_u16(),
                unix_start,
                elapsed,
            ))
            .is_err()
        {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
    response
}

fn span(method: &str, status: u16, start: u128, elapsed: Duration) -> serde_json::Value {
    serde_json::json!({
        "traceId": uuid::Uuid::new_v4().simple().to_string(),
        "spanId": &uuid::Uuid::new_v4().simple().to_string()[..16],
        "name": format!("HTTP {method}"), "kind": 2,
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + elapsed.as_nanos()).to_string(),
        "attributes": [
            {"key":"http.request.method", "value":{"stringValue":method}},
            {"key":"http.response.status_code", "value":{"intValue":status.to_string()}}
        ],
        "status": {"code": if status >= 500 {2} else {0}}
    })
}

fn payload(spans: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"resourceSpans":[{
        "resource":{"attributes":[{"key":"service.name","value":{"stringValue":"sift-server"}}]},
        "scopeSpans":[{"scope":{"name":"sift.http","version":crate::VERSION},"spans":spans}]
    }]})
}

/// Starts a bounded best-effort exporter. The endpoint is the complete traces
/// URL; no implicit suffix, environment credentials, redirects or URL logging.
pub fn configure_otlp(endpoint: Option<&str>) -> anyhow::Result<()> {
    let Some(endpoint) = endpoint else {
        return Ok(());
    };
    let endpoint =
        url::Url::parse(endpoint).map_err(|_| anyhow::anyhow!("invalid OTLP endpoint"))?;
    let loopback = endpoint.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    anyhow::ensure!(
        (endpoint.scheme() == "https" || (endpoint.scheme() == "http" && loopback))
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "OTLP endpoint requires HTTPS (or loopback HTTP) without credentials, query or fragment"
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    EXPORTER
        .set(tx)
        .map_err(|_| anyhow::anyhow!("OTLP exporter is already configured"))?;
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut spans = vec![first];
            while spans.len() < 32 {
                match rx.try_recv() {
                    Ok(span) => spans.push(span),
                    Err(_) => break,
                }
            }
            let count = spans.len() as u64;
            if !export_batch(&client, &endpoint, spans).await {
                DROPPED.fetch_add(count, Ordering::Relaxed);
            }
        }
    });
    Ok(())
}

async fn export_batch(
    client: &reqwest::Client,
    endpoint: &url::Url,
    spans: Vec<serde_json::Value>,
) -> bool {
    let Ok(mut response) = client
        .post(endpoint.clone())
        .json(&payload(spans))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len().saturating_add(chunk.len()) <= 64 * 1024 => {
                body.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return false,
        }
    }
    if body.is_empty() {
        return true;
    }
    let Ok(response) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return false;
    };
    let rejected = &response["partialSuccess"]["rejectedSpans"];
    rejected.is_null() || rejected.as_u64() == Some(0) || rejected.as_str() == Some("0")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn otlp_http_delivers_to_a_collector_and_reports_rejection() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let app = axum::Router::new()
            .route(
                "/partial",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({"partialSuccess":{"rejectedSpans":"1"}}))
                }),
            )
            .route(
                "/traces",
                axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let tx = tx.clone();
                    async move {
                        tx.send(body).await.unwrap();
                        axum::Json(serde_json::json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let endpoint = url::Url::parse(&format!("http://{address}/traces")).unwrap();
        assert!(
            export_batch(
                &client,
                &endpoint,
                vec![span("POST", 200, 1, Duration::from_millis(1))]
            )
            .await
        );
        let received = rx.recv().await.unwrap();
        assert_eq!(
            received["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"],
            "HTTP POST"
        );
        assert!(!export_batch(&client, &endpoint.join("/missing").unwrap(), vec![]).await);
        assert!(!export_batch(&client, &endpoint.join("/partial").unwrap(), vec![]).await);
        task.abort();
    }
    #[test]
    fn metrics_are_bounded_and_otlp_has_only_allowlisted_fields() {
        let metrics = Metrics::default();
        metrics.record(200, Duration::from_millis(5));
        metrics.record(503, Duration::from_secs(2));
        let output = metrics.render();
        assert!(output.contains("sift_http_requests_total{status_class=\"2xx\"} 1"));
        assert!(output.contains("sift_http_request_duration_seconds_bucket{le=\"0.005\"} 1"));
        assert!(output.contains("sift_http_request_duration_seconds_count 2"));
        let p = payload(vec![span("GET", 503, 123, Duration::from_nanos(10))]);
        let s = &p["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(s["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(s["spanId"].as_str().unwrap().len(), 16);
        assert_eq!(s["endTimeUnixNano"], "133");
        assert_eq!(s["status"]["code"], 2);
        assert_eq!(s["attributes"].as_array().unwrap().len(), 2);
    }
}
