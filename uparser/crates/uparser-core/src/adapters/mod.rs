//! `ProtocolAdapter` trait and registry, per ARCHITECTURE.md §2.0/§2.1.

pub mod dots_ocr;
pub mod generic_vlm;
pub mod local_tesseract;
pub mod mineru_vlm;
pub mod mock;
pub mod monkeyocr_v2;
#[cfg(feature = "native")]
pub mod native;
#[cfg(feature = "pipeline-local-table")]
pub mod onnx_table;
pub mod paddleocr;
pub mod paddlex_structure;
pub mod pipeline;
pub mod pipeline_serving;

use crate::ingest::RenderedPage;
use crate::testing::MockDispatch;
use crate::transport::{ChatCompletionRequest, RestRequest, Transport, TransportError};
use crate::types::{Block, PageError};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// How an adapter's raw model output is encoded, before `output_parse.rs`
/// (a later phase) turns it into `Block`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawOutputFormat {
    CustomToken,
    StrictJson,
    PythonLiteralEval,
    OcrBoxes,
    /// No wire format at all — a zero-model adapter (e.g. `native`) that
    /// doesn't dispatch any request to parse.
    None,
}

/// Which enhanced-IR signals an adapter is able to emit, gating whether
/// `postprocess.rs` (a later phase) can run its signal-enhanced layer or
/// must degrade to the pure-geometry layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostprocessSignals {
    pub spans: bool,
    pub merge_hint: bool,
    pub font_size: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceHint {
    Lightweight,
    Heavy,
}

#[derive(Debug, Clone)]
pub struct LocalModelSpec {
    pub model_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteEndpointSpec {
    pub endpoint_env_var: &'static str,
}

#[derive(Debug, Clone)]
pub enum StageBackend {
    Local(LocalModelSpec),
    Remote(RemoteEndpointSpec),
}

/// A `ModelStage` is an independently deployable backend unit — not a
/// request round. mineru-vlm's two HTTP round-trips against one vLLM
/// endpoint are a single stage; pipeline's layout/ocr/formula/table are
/// four.
#[derive(Debug, Clone)]
pub struct ModelStage {
    pub stage_name: &'static str,
    pub default_backend: StageBackend,
    pub allows_local: bool,
    pub resource_hint: ResourceHint,
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("no mock response seeded for key {0:?}")]
    MockKeyMissing(String),
    #[error("request cancelled")]
    Cancelled,
}

/// Where `ParseCtx::dispatch` sends its requests: a real HTTP transport,
/// or (in tests) a `MockDispatch` keyed by the request's `endpoint`
/// field — letting a two-stage adapter's orchestration be exercised
/// fully offline (the automatable part of Gate G1).
enum Dispatcher {
    Real(Arc<Transport>),
    Mock(Arc<MockDispatch>),
}

/// Per-page context handed to an adapter's `parse_page`. Wraps the
/// dispatcher, the document-level concurrency budget, and (in later
/// phases) the cache.
pub struct ParseCtx {
    dispatcher: Dispatcher,
    pub permits: Arc<Semaphore>,
    warnings: Arc<Mutex<Vec<String>>>,
    cancellation: crate::frontend::CancellationToken,
}

impl ParseCtx {
    pub fn new(transport: Arc<Transport>, permits: Arc<Semaphore>) -> Self {
        Self {
            dispatcher: Dispatcher::Real(transport),
            permits,
            warnings: Arc::new(Mutex::new(Vec::new())),
            cancellation: crate::frontend::CancellationToken::default(),
        }
    }

    /// Build a `ParseCtx` backed by a `MockDispatch` instead of a real
    /// HTTP transport, for offline adapter tests.
    pub fn with_mock(mock: Arc<MockDispatch>, permits: Arc<Semaphore>) -> Self {
        Self {
            dispatcher: Dispatcher::Mock(mock),
            permits,
            warnings: Arc::new(Mutex::new(Vec::new())),
            cancellation: crate::frontend::CancellationToken::default(),
        }
    }

    /// Same as `new`, but sharing an externally-owned warnings collector
    /// instead of each `ParseCtx` getting its own (used by `scheduler.rs`
    /// so every page's adapter accumulates into one document-level sink
    /// that survives past any single page's `ParseCtx` being dropped).
    pub fn new_with_shared_warnings(
        transport: Arc<Transport>,
        permits: Arc<Semaphore>,
        warnings: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            dispatcher: Dispatcher::Real(transport),
            permits,
            warnings,
            cancellation: crate::frontend::CancellationToken::default(),
        }
    }

    pub fn new_with_cancellation(
        transport: Arc<Transport>,
        permits: Arc<Semaphore>,
        warnings: Arc<Mutex<Vec<String>>>,
        cancellation: crate::frontend::CancellationToken,
    ) -> Self {
        Self {
            dispatcher: Dispatcher::Real(transport),
            permits,
            warnings,
            cancellation,
        }
    }

    /// Record a non-fatal recovery/degradation warning: printed to
    /// stderr immediately (so a live CLI run stays informative) and also
    /// collected so `ParseResult.warnings` can carry it to callers that
    /// aren't watching stderr — e.g. the `api.rs`/Node/Python binding
    /// path, which previously had no channel for this at all (every
    /// adapter's category-mapping/output-recovery warning only ever
    /// reached a bare `eprintln!`, invisible to non-interactive callers).
    pub fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        eprintln!("{message}");
        self.warnings
            .lock()
            .expect("warnings mutex not poisoned")
            .push(message);
    }

    /// Everything recorded via `warn()` on this `ParseCtx` so far (or,
    /// when constructed via `new_with_shared_warnings`, on every
    /// `ParseCtx` sharing the same collector).
    pub fn warnings_snapshot(&self) -> Vec<String> {
        self.warnings
            .lock()
            .expect("warnings mutex not poisoned")
            .clone()
    }

    /// Acquire a slot from the document-level concurrency budget.
    pub async fn acquire_permit(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.permits
            .acquire()
            .await
            .expect("semaphore never closed")
    }

    /// Dispatch a chat-completion request. In `Dispatcher::Mock` mode,
    /// the request's `endpoint` field is used as the seed key.
    pub async fn dispatch(&self, req: ChatCompletionRequest) -> Result<Value, DispatchError> {
        if self.cancellation.is_cancelled() {
            return Err(DispatchError::Cancelled);
        }
        match &self.dispatcher {
            Dispatcher::Real(transport) => tokio::select! {
                result = transport.dispatch(req) => Ok(result?),
                _ = self.cancellation.cancelled() => Err(DispatchError::Cancelled),
            },
            Dispatcher::Mock(mock) => mock
                .dispatch(&req.endpoint)
                .ok_or_else(|| DispatchError::MockKeyMissing(req.endpoint.clone())),
        }
    }

    /// Dispatch a raw-JSON REST request (Pipeline Model Serving's
    /// contract). In `Dispatcher::Mock` mode, `endpoint` is used as the
    /// seed key — same mechanism as `dispatch`, no changes needed to
    /// `MockDispatch` itself.
    ///
    /// `timeout`/`max_retries` are the *caller's own* declared values
    /// (e.g. `PipelineAdapter.timeout`/`PaddleOcrAdapter.max_retries`) —
    /// previously this hardcoded `60s`/`2` regardless of what an adapter
    /// struct actually declared, silently making those two fields dead
    /// code no configuration attempt could ever affect (see C.4 in
    /// `CLI_ENHANCEMENT_PROPOSAL.md`).
    pub async fn dispatch_rest(
        &self,
        endpoint: &str,
        body: Value,
        timeout: std::time::Duration,
        max_retries: u32,
    ) -> Result<Value, DispatchError> {
        if self.cancellation.is_cancelled() {
            return Err(DispatchError::Cancelled);
        }
        match &self.dispatcher {
            Dispatcher::Real(transport) => tokio::select! {
                result = transport.dispatch_rest(RestRequest {
                    endpoint: endpoint.to_string(),
                    body,
                    timeout,
                    max_retries,
                }) => Ok(result?),
                _ = self.cancellation.cancelled() => Err(DispatchError::Cancelled),
            },
            Dispatcher::Mock(mock) => mock
                .dispatch(endpoint)
                .ok_or_else(|| DispatchError::MockKeyMissing(endpoint.to_string())),
        }
    }

    /// Decode a page's PNG bytes and crop to `bbox_px`, returning an RGB
    /// buffer ready for `imaging::resize_by_need`/re-encoding.
    pub fn crop(&self, page: &RenderedPage, bbox_px: [i32; 4]) -> Result<image::RgbImage, String> {
        let img = image::load_from_memory(&page.png_bytes).map_err(|e| e.to_string())?;
        crate::imaging::crop(&crate::imaging::to_rgb(&img), bbox_px)
            .ok_or_else(|| format!("crop region {bbox_px:?} does not overlap the page at all"))
    }
}

#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn spec(&self) -> &'static crate::protocol_spec::ProtocolSpec {
        crate::protocol_spec::get(self.name())
            .expect("every registered protocol adapter must have a ProtocolSpec")
    }
    fn coordinate_system(&self) -> crate::types::CoordinateSystem;
    fn provides_reading_order(&self) -> bool;
    fn category_vocab(&self) -> &[&'static str];
    fn raw_output_format(&self) -> RawOutputFormat;
    fn emitted_signals(&self) -> PostprocessSignals;
    fn model_stages(&self) -> Vec<ModelStage>;

    async fn parse_page(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError>;
}

/// Whether a `pipeline` stage runs in-process (`ort`, table only by
/// default) or against a Pipeline Model Serving endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[value(rename_all = "lowercase")]
pub enum StageBackendChoice {
    Local,
    Remote,
}

/// Per-stage backend/endpoint overrides for the `pipeline` protocol
/// (T-5.1). `None` fields fall back to `PipelineAdapter::default()`'s
/// per-stage default (ARCHITECTURE.md §11.2: `table` defaults `Local`,
/// the other three default `Remote`).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PipelineConfig {
    pub layout_backend: Option<StageBackendChoice>,
    pub layout_endpoint: Option<String>,
    pub ocr_backend: Option<StageBackendChoice>,
    pub ocr_endpoint: Option<String>,
    pub formula_backend: Option<StageBackendChoice>,
    pub formula_endpoint: Option<String>,
    pub table_backend: Option<StageBackendChoice>,
    pub table_model_path: Option<String>,
}

/// Endpoint/model overrides applied on top of an adapter's
/// `Default::default()` when the CLI's `--endpoint`/`--model` flags are
/// given. Adapters with no such fields (`mock`, `native`) ignore these.
#[derive(Debug, Clone, Default)]
pub struct AdapterOverrides {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    /// `pipeline`-only stage-backend overrides; ignored by every other
    /// adapter.
    pub pipeline: Option<PipelineConfig>,
}

type AdapterFactory = Box<dyn Fn(&AdapterOverrides) -> Arc<dyn ProtocolAdapter> + Send + Sync>;

/// `name -> adapter factory` lookup table. Stores constructors rather
/// than pre-built instances so a real endpoint/model can be substituted
/// per-invocation without needing `Any`/downcasting on the trait object.
#[derive(Default)]
pub struct Registry {
    factories: HashMap<&'static str, AdapterFactory>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        name: &'static str,
        factory: impl Fn(&AdapterOverrides) -> Arc<dyn ProtocolAdapter> + Send + Sync + 'static,
    ) {
        self.factories.insert(name, Box::new(factory));
    }

    /// Every registered adapter name (T-9.4's `uparser protocols`).
    pub fn names(&self) -> Vec<&'static str> {
        self.factories.keys().copied().collect()
    }

    /// Build an adapter instance by name, applying `overrides`. Returns
    /// `None` for an unregistered name.
    pub fn build(
        &self,
        name: &str,
        overrides: &AdapterOverrides,
    ) -> Option<Arc<dyn ProtocolAdapter>> {
        self.factories.get(name).map(|factory| factory(overrides))
    }

    /// Registry pre-populated with every built-in adapter's factory.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();

        registry.register("mock", |_overrides| Arc::new(mock::MockAdapter::default()));

        registry.register("tesseract", |_overrides| {
            Arc::new(local_tesseract::TesseractAdapter::default())
        });

        registry.register("mineru-vlm", |overrides| {
            let mut adapter = mineru_vlm::MineruVlmAdapter::default();
            if let Some(endpoint) = &overrides.endpoint {
                adapter.endpoint_base = endpoint.clone();
            }
            if let Some(model) = &overrides.model {
                adapter.model = model.clone();
            }
            Arc::new(adapter)
        });

        registry.register("dots-ocr", |overrides| {
            let mut adapter = dots_ocr::DotsOcrAdapter::default();
            if let Some(endpoint) = &overrides.endpoint {
                adapter.endpoint_base = endpoint.clone();
            }
            if let Some(model) = &overrides.model {
                adapter.model = model.clone();
            }
            Arc::new(adapter)
        });

        registry.register("generic-vlm", |overrides| {
            let mut adapter = generic_vlm::GenericVlmAdapter::default();
            if let Some(endpoint) = &overrides.endpoint {
                adapter.endpoint = endpoint.clone();
            }
            if let Some(model) = &overrides.model {
                adapter.model = model.clone();
            }
            Arc::new(adapter)
        });

        registry.register("monkeyocr-v2", |overrides| {
            let mut adapter = monkeyocr_v2::MonkeyOcrV2Adapter::default();
            if let Some(endpoint) = &overrides.endpoint {
                adapter.endpoint_base = endpoint.clone();
            }
            if let Some(model) = &overrides.model {
                adapter.model = model.clone();
            }
            Arc::new(adapter)
        });

        #[cfg(feature = "native")]
        registry.register("native", |_overrides| Arc::new(native::NativeAdapter));

        registry.register("paddleocr", |overrides| {
            let mut adapter = paddleocr::PaddleOcrAdapter::default();
            if let Some(endpoint) = &overrides.endpoint {
                adapter.endpoint = endpoint.clone();
            }
            Arc::new(adapter)
        });

        registry.register("paddlex-structure", |overrides| {
            let mut adapter = paddlex_structure::PaddleXStructureAdapter::default();
            if let Some(endpoint) = &overrides.endpoint {
                adapter.endpoint = endpoint.clone();
            }
            Arc::new(adapter)
        });

        registry.register("pipeline", |overrides| {
            let mut adapter = pipeline::PipelineAdapter::default();
            if let Some(cfg) = &overrides.pipeline {
                adapter.apply_config(cfg);
            }
            Arc::new(adapter)
        });

        registry
    }
}

/// Extract `choices[0].message.content` from an OpenAI-chat-completions-shaped
/// response, the shape shared by `mineru-vlm`/`dots-ocr`/`monkeyocr-v2`.
///
/// Unlike a plain `.as_str().unwrap_or("")`, a missing/null content field is
/// treated as an error rather than silently coerced into "the model
/// legitimately returned an empty string" — a malformed envelope, a content
/// filter refusal, or a `finish_reason` other than `"stop"` all land here,
/// and coercing to `""` previously made layout detection quietly produce
/// zero blocks for the whole page instead of surfacing the real cause.
pub fn extract_chat_content(resp: &Value) -> Result<&str, String> {
    if let Some(content) = resp["choices"][0]["message"]["content"].as_str() {
        return Ok(content);
    }
    if let Some(err) = resp.get("error") {
        return Err(format!("backend returned an error: {err}"));
    }
    let Some(choice) = resp.get("choices").and_then(|c| c.get(0)) else {
        return Err(format!(
            "response is missing choices[0].message.content: {resp}"
        ));
    };
    if let Some(finish_reason) = choice["finish_reason"].as_str()
        && finish_reason != "stop"
    {
        return Err(format!(
            "response has no message content (finish_reason: {finish_reason:?})"
        ));
    }
    if choice["message"]["content"].is_null() {
        return Err("response message content is null".to_string());
    }
    Err(format!(
        "response is missing choices[0].message.content: {resp}"
    ))
}

/// True if a chat-completions response's `finish_reason` is `"length"`
/// — the model hit its `max_tokens` budget and the returned content is a
/// truncated prefix, not a complete response. Distinct from
/// `extract_chat_content`'s error path: content is still present and
/// usable (the fault-tolerant `output_parse.rs` parsers can often
/// recover a valid prefix from it), so this is a warning-level signal
/// for the caller to surface, not a `PageError`. Previously nothing
/// checked this at all for content-bearing responses — a
/// version/document combination that reliably truncates would fail
/// silently, with the tolerant parser quietly returning whatever
/// partial structure it could recover from the cut-off text and no
/// indication that truncation (not malformed output) was the cause (see
/// D.12 in `CLI_ENHANCEMENT_PROPOSAL.md`).
pub fn is_truncated_response(resp: &Value) -> bool {
    resp["choices"][0]["finish_reason"].as_str() == Some("length")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::RenderedPage;
    use crate::transport::Transport;

    #[tokio::test]
    async fn registry_lookup_and_parse_page() {
        let registry = Registry::with_builtins();
        let adapter = registry
            .build("mock", &AdapterOverrides::default())
            .expect("mock adapter registered");

        let ctx = ParseCtx::new(Arc::new(Transport::new()), Arc::new(Semaphore::new(1)));
        let page = RenderedPage {
            page_num: 1,
            width: 10,
            height: 10,
            png_bytes: vec![],
        };

        let blocks = adapter.parse_page(&page, &ctx).await.unwrap();
        // Raw (unmerged) mock output is 2 blocks — see mock.rs's doc
        // comment; postprocess.rs merges them back to 1 downstream.
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn extract_chat_content_returns_the_message_content() {
        let resp = serde_json::json!({
            "choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}]
        });
        assert_eq!(extract_chat_content(&resp).unwrap(), "hello");
    }

    #[test]
    fn extract_chat_content_surfaces_backend_error_object() {
        let resp = serde_json::json!({"error": {"message": "content filter triggered"}});
        let err = extract_chat_content(&resp).unwrap_err();
        assert!(err.contains("content filter triggered"), "{err}");
    }

    #[test]
    fn extract_chat_content_surfaces_non_stop_finish_reason() {
        let resp = serde_json::json!({
            "choices": [{"message": {}, "finish_reason": "content_filter"}]
        });
        let err = extract_chat_content(&resp).unwrap_err();
        assert!(err.contains("content_filter"), "{err}");
    }

    #[test]
    fn extract_chat_content_surfaces_null_content() {
        let resp = serde_json::json!({
            "choices": [{"message": {"content": null}, "finish_reason": "stop"}]
        });
        let err = extract_chat_content(&resp).unwrap_err();
        assert!(err.contains("null"), "{err}");
    }

    #[test]
    fn extract_chat_content_surfaces_missing_field_with_diagnostic() {
        let resp = serde_json::json!({"unexpected": "shape"});
        let err = extract_chat_content(&resp).unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn is_truncated_response_detects_length_finish_reason() {
        let resp = serde_json::json!({
            "choices": [{"message": {"content": "partial..."}, "finish_reason": "length"}]
        });
        assert!(is_truncated_response(&resp));
    }

    #[test]
    fn is_truncated_response_false_for_stop() {
        let resp = serde_json::json!({
            "choices": [{"message": {"content": "done"}, "finish_reason": "stop"}]
        });
        assert!(!is_truncated_response(&resp));
    }

    #[test]
    fn is_truncated_response_false_for_missing_finish_reason() {
        let resp = serde_json::json!({"choices": [{"message": {"content": "x"}}]});
        assert!(!is_truncated_response(&resp));
    }

    #[tokio::test]
    async fn dispatch_rest_honors_the_callers_own_timeout_not_a_hardcoded_one() {
        // C.4: `dispatch_rest` previously hardcoded `timeout: 60s`
        // regardless of what was passed in — a caller-supplied short
        // timeout would have been silently ignored and this test would
        // hang for 60s waiting on the slow response instead of failing
        // fast. Proves the parameter genuinely reaches `Transport`.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(2)))
            .mount(&server)
            .await;

        let ctx = ParseCtx::new(Arc::new(Transport::new()), Arc::new(Semaphore::new(1)));
        let start = std::time::Instant::now();
        let result = ctx
            .dispatch_rest(
                &format!("{}/slow", server.uri()),
                serde_json::json!({}),
                std::time::Duration::from_millis(100),
                0,
            )
            .await;
        assert!(result.is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "expected the 100ms timeout to be honored, took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_dispatch() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(2)))
            .mount(&server)
            .await;
        let cancellation = crate::frontend::CancellationToken::default();
        let ctx = ParseCtx::new_with_cancellation(
            Arc::new(Transport::new()),
            Arc::new(Semaphore::new(1)),
            Arc::new(Mutex::new(Vec::new())),
            cancellation.clone(),
        );
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancellation.cancel();
        });

        let start = std::time::Instant::now();
        let result = ctx
            .dispatch_rest(
                &format!("{}/slow", server.uri()),
                serde_json::json!({}),
                std::time::Duration::from_secs(5),
                0,
            )
            .await;
        assert!(matches!(result, Err(DispatchError::Cancelled)));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
}
