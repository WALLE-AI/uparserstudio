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
    #[error("request failed after {attempts} attempts: {source}")]
    Exhausted {
        attempts: u32,
        #[source]
        source: reqwest::Error,
    },
    #[error("server returned status {status}: {body}")]
    ServerError { status: u16, body: String },
    #[error(transparent)]
    Request(#[from] reqwest::Error),
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
        let mut attempt = 0;
        let mut last_err: reqwest::Error;
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
                        return Ok(resp.json::<Value>().await?);
                    }
                    if status.is_server_error() && attempt <= max_retries {
                        let backoff = Duration::from_millis(50 * 2u64.pow(attempt - 1));
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
                    last_err = err;
                    if attempt <= max_retries {
                        let backoff = Duration::from_millis(50 * 2u64.pow(attempt - 1));
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    break;
                }
            }
        }

        Err(TransportError::Exhausted {
            attempts: attempt,
            source: last_err,
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
