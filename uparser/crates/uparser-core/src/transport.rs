//! reqwest+tokio client for OpenAI-compatible chat-completions endpoints,
//! with retry/backoff, timeout, and a document-level concurrency limiter.
//! Per ARCHITECTURE.md §9.2 / T-0.4.

use base64::Engine as _;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Every retry attempt failed — could be a network error, a
    /// consistently-retryable HTTP status (429/5xx), or a `200` response
    /// whose body never parsed as valid JSON (C.3: a gateway that
    /// truncates or substitutes an HTML error page while still reporting
    /// `200`) on every attempt. `message` carries the last attempt's
    /// specific cause.
    #[error("request failed after {attempts} attempts: {message}")]
    Exhausted { attempts: u32, message: String },
    #[error("server returned status {status}: {body}")]
    ServerError { status: u16, body: String },
    /// The overall dispatch (across all retry attempts) exceeded the
    /// hard wall-clock backstop — a safety net independent of whatever
    /// `timeout`/`max_retries` an adapter configured, since those two
    /// multiply together and a large combination of both could otherwise
    /// hang a single page indefinitely (C.5).
    #[error("dispatch exceeded the overall {limit:?} wall-clock backstop")]
    OverallTimeout { limit: Duration },
    #[error(transparent)]
    Request(#[from] reqwest::Error),
}

/// Cap on exponential backoff growth — without this, the Nth retry sleeps
/// `50ms * 2^(N-1)`, e.g. ~25.6s by the 10th attempt (C.2).
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Sanity cap on how long a server's own `Retry-After` header is honored
/// for — a misbehaving/malicious backend sending an enormous value
/// shouldn't be able to stall a page indefinitely.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Hard backstop on total dispatch duration across every retry attempt,
/// independent of the per-adapter `timeout`/`max_retries` configuration
/// (C.5) — those two multiply together, so a large combination of both
/// (whether misconfigured or just genuinely large) could otherwise hang
/// a single page's request indefinitely with no upper bound at all.
const OVERALL_DISPATCH_TIMEOUT: Duration = Duration::from_secs(600);

/// "Full jitter" backoff (per AWS's well-known backoff-and-jitter
/// writeup): sleep a random duration in `[0, min(cap, base))` rather
/// than a fixed deterministic delay, so concurrently-failing requests
/// don't all wake up and retry in lockstep against a backend that's
/// still recovering (C.2). Uses wall-clock nanoseconds as a cheap
/// entropy source — this is scheduling jitter, not a security-sensitive
/// value, so a `rand`-crate dependency isn't warranted for it.
fn jittered_backoff(attempt: u32) -> Duration {
    let base_ms = 50u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(20));
    let capped_ms = base_ms.min(MAX_BACKOFF.as_millis() as u64);
    let entropy = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jittered_ms = entropy % (capped_ms + 1);
    Duration::from_millis(jittered_ms)
}

/// A `Retry-After` header value, in whichever of the two HTTP-standard
/// forms it uses: an integer number of seconds, or (unhandled here — see
/// below) an HTTP-date. Only the numeric-seconds form is parsed; an
/// HTTP-date `Retry-After` falls back to the computed jittered backoff
/// instead, which is a safe (if suboptimal) degradation rather than a
/// parsing failure.
fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: u64 = value.trim().parse().ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// A generic (non-chat-completions) REST request — used by `pipeline`'s
/// Remote stages, which talk to Pipeline Model Serving's own lightweight
/// contract (ARCHITECTURE.md §11.3), not an OpenAI-shaped endpoint.
pub struct RestRequest {
    pub endpoint: String,
    pub body: Value,
    pub timeout: Duration,
    pub max_retries: u32,
}

pub struct ChatCompletionRequest {
    pub endpoint: String,
    pub model: String,
    pub messages: Vec<Value>,
    /// Sampling params (temperature, top_p, ...) merged verbatim into the
    /// request body.
    pub sampling: Value,
    pub timeout: Duration,
    pub max_retries: u32,
}

/// Encode PNG bytes as a `data:image/png;base64,...` URL for embedding in
/// a chat-completions image content part.
pub fn image_data_url(png_bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    format!("data:image/png;base64,{encoded}")
}

pub struct Transport {
    client: reqwest::Client,
    /// Document-level concurrency budget, shared with the scheduler.
    semaphore: Arc<Semaphore>,
}

impl Transport {
    pub fn new() -> Self {
        Self::with_concurrency(Semaphore::MAX_PERMITS)
    }

    pub fn with_concurrency(max_concurrency: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            semaphore: Arc::new(Semaphore::new(max_concurrency)),
        }
    }

    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    pub async fn dispatch(&self, req: ChatCompletionRequest) -> Result<Value, TransportError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .expect("semaphore never closed");

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": req.messages,
        });
        if let Value::Object(sampling) = &req.sampling
            && let Value::Object(map) = &mut body
        {
            for (k, v) in sampling {
                map.insert(k.clone(), v.clone());
            }
        }

        self.post_with_retry(&req.endpoint, body, req.timeout, req.max_retries)
            .await
    }

    /// Dispatch a raw-JSON REST request (Pipeline Model Serving's own
    /// contract, ARCHITECTURE.md §11.3 — not chat-completions-shaped).
    /// Shares the same retry/backoff/concurrency skeleton as `dispatch`.
    pub async fn dispatch_rest(&self, req: RestRequest) -> Result<Value, TransportError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .expect("semaphore never closed");

        self.post_with_retry(&req.endpoint, req.body, req.timeout, req.max_retries)
            .await
    }

    async fn post_with_retry(
        &self,
        endpoint: &str,
        body: Value,
        timeout: Duration,
        max_retries: u32,
    ) -> Result<Value, TransportError> {
        match tokio::time::timeout(
            OVERALL_DISPATCH_TIMEOUT,
            self.post_with_retry_inner(endpoint, body, timeout, max_retries),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(TransportError::OverallTimeout {
                limit: OVERALL_DISPATCH_TIMEOUT,
            }),
        }
    }

    async fn post_with_retry_inner(
        &self,
        endpoint: &str,
        body: Value,
        timeout: Duration,
        max_retries: u32,
    ) -> Result<Value, TransportError> {
        let mut attempt = 0;
        let mut last_message: String;
        loop {
            attempt += 1;
            let result = self
                .client
                .post(endpoint)
                .timeout(timeout)
                .json(&body)
                .send()
                .await;

            match result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        // C.3: a `200` with a body that isn't valid JSON
                        // (a gateway truncating the response, or
                        // substituting an HTML error page while still
                        // reporting success) is treated as retryable —
                        // previously this returned `Err` immediately via
                        // `?`, consuming none of the retry budget meant
                        // for exactly this kind of transient backend
                        // flakiness.
                        match resp.json::<Value>().await {
                            Ok(json) => return Ok(json),
                            Err(err) => {
                                last_message = format!("200 response was not valid JSON: {err}");
                                if attempt <= max_retries {
                                    tokio::time::sleep(jittered_backoff(attempt)).await;
                                    continue;
                                }
                                break;
                            }
                        }
                    }
                    // C.1: 429 (rate limiting) is now retried alongside
                    // 5xx — previously only server errors were retried,
                    // but 429 is the single most common failure mode
                    // under real LLM/OCR backend load.
                    let retryable = status.is_server_error() || status.as_u16() == 429;
                    if retryable && attempt <= max_retries {
                        let backoff = retry_after_from_headers(resp.headers())
                            .unwrap_or_else(|| jittered_backoff(attempt));
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    let text = resp.text().await.unwrap_or_default();
                    return Err(TransportError::ServerError {
                        status: status.as_u16(),
                        body: text,
                    });
                }
                Err(err) => {
                    last_message = err.to_string();
                    if attempt <= max_retries {
                        tokio::time::sleep(jittered_backoff(attempt)).await;
                        continue;
                    }
                    break;
                }
            }
        }

        Err(TransportError::Exhausted {
            attempts: attempt,
            message: last_message,
        })
    }
}

impl Default for Transport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(endpoint: String) -> ChatCompletionRequest {
        ChatCompletionRequest {
            endpoint,
            model: "test-model".into(),
            messages: vec![],
            sampling: serde_json::json!({}),
            timeout: Duration::from_secs(2),
            max_retries: 2,
        }
    }

    #[test]
    fn image_data_url_has_expected_prefix() {
        let url = image_data_url(&[1, 2, 3]);
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn dispatch_succeeds_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let transport = Transport::new();
        let result = transport
            .dispatch(req(format!("{}/chat", server.uri())))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn dispatch_retries_on_500_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let transport = Transport::new();
        let result = transport
            .dispatch(req(format!("{}/chat", server.uri())))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn dispatch_retries_on_429_then_succeeds() {
        // C.1: 429 previously wasn't retried at all — only 5xx was.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let transport = Transport::new();
        let result = transport
            .dispatch(req(format!("{}/chat", server.uri())))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn dispatch_honors_retry_after_header_on_429() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let transport = Transport::new();
        let result = transport
            .dispatch(req(format!("{}/chat", server.uri())))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn dispatch_exhausts_retries_on_persistent_429_with_clear_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let mut r = req(format!("{}/chat", server.uri()));
        r.max_retries = 1;
        let transport = Transport::new();
        let err = transport.dispatch(r).await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::ServerError { status: 429, .. }
        ));
    }

    #[tokio::test]
    async fn dispatch_retries_on_200_with_invalid_json_body() {
        // C.3: a `200` whose body isn't valid JSON (gateway truncation,
        // or a substituted HTML error page) previously failed outright
        // with zero retry budget spent on it.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html>not json</html>")
                    .append_header("content-type", "text/html"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let transport = Transport::new();
        let result = transport
            .dispatch(req(format!("{}/chat", server.uri())))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn dispatch_exhausts_retries_on_persistent_invalid_json_with_clear_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let mut r = req(format!("{}/chat", server.uri()));
        r.max_retries = 1;
        let transport = Transport::new();
        let err = transport.dispatch(r).await.unwrap_err();
        match err {
            TransportError::Exhausted { attempts, message } => {
                assert_eq!(attempts, 2);
                assert!(message.contains("not valid JSON"), "{message}");
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    #[test]
    fn jittered_backoff_never_exceeds_the_cap() {
        for attempt in 1..=25 {
            let d = jittered_backoff(attempt);
            assert!(
                d <= MAX_BACKOFF,
                "attempt {attempt} produced {d:?} > cap {MAX_BACKOFF:?}"
            );
        }
    }

    #[test]
    fn retry_after_header_is_parsed_and_capped() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_secs(3))
        );

        let mut huge = reqwest::header::HeaderMap::new();
        huge.insert(reqwest::header::RETRY_AFTER, "999999".parse().unwrap());
        assert_eq!(retry_after_from_headers(&huge), Some(MAX_RETRY_AFTER));

        let mut malformed = reqwest::header::HeaderMap::new();
        malformed.insert(
            reqwest::header::RETRY_AFTER,
            "not-a-number".parse().unwrap(),
        );
        assert_eq!(retry_after_from_headers(&malformed), None);

        let none = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_from_headers(&none), None);
    }

    #[test]
    fn overall_timeout_error_message_names_the_limit() {
        // Not exercising the real 600s backstop live (impractically
        // slow for a unit test) — this proves the error variant/message
        // are wired up correctly; the `tokio::time::timeout` wrapping
        // mechanism itself is standard library behavior, not something
        // this project needs to re-verify.
        let err = TransportError::OverallTimeout {
            limit: OVERALL_DISPATCH_TIMEOUT,
        };
        assert!(err.to_string().contains("600s"), "{err}");
    }

    #[tokio::test]
    async fn dispatch_times_out_on_slow_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(300)))
            .mount(&server)
            .await;

        let mut r = req(format!("{}/chat", server.uri()));
        r.timeout = Duration::from_millis(50);
        r.max_retries = 0;

        let transport = Transport::new();
        let result = transport.dispatch(r).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dispatch_rest_posts_raw_body_and_succeeds_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/layout"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"boxes": []})),
            )
            .mount(&server)
            .await;

        let transport = Transport::new();
        let result = transport
            .dispatch_rest(RestRequest {
                endpoint: format!("{}/layout", server.uri()),
                body: serde_json::json!({"task": "layout"}),
                timeout: Duration::from_secs(2),
                max_retries: 1,
            })
            .await
            .unwrap();
        assert_eq!(result["boxes"], serde_json::json!([]));
    }

    /// Indirect check: with concurrency capped to 2 and each request taking
    /// ~50ms, 6 requests must run in >=3 serialized batches. If the
    /// semaphore weren't limiting concurrency, all 6 would complete in
    /// ~1 batch (~50ms) instead of ~3 batches (~150ms).
    #[tokio::test]
    async fn concurrency_cap_is_respected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true}))
                    .set_delay(Duration::from_millis(50)),
            )
            .mount(&server)
            .await;

        let transport = Arc::new(Transport::with_concurrency(2));
        let start = std::time::Instant::now();

        let mut handles = vec![];
        for _ in 0..6 {
            let transport = Arc::clone(&transport);
            let endpoint = format!("{}/chat", server.uri());
            handles.push(tokio::spawn(async move {
                transport.dispatch(req(endpoint)).await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        assert!(
            start.elapsed() >= Duration::from_millis(130),
            "expected >=3 serialized batches of ~50ms with concurrency=2, elapsed={:?}",
            start.elapsed()
        );
    }
}
