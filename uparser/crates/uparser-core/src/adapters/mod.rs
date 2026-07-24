//! `ProtocolAdapter` trait and registry, per ARCHITECTURE.md §2.0/§2.1.

pub mod dots_ocr;
pub mod mineru_vlm;
pub mod mock;
pub mod monkeyocr_v2;
#[cfg(feature = "native")]
pub mod native;
#[cfg(feature = "pipeline-local-table")]
pub mod onnx_table;
pub mod paddleocr;
pub mod pipeline;
pub mod pipeline_serving;

use crate::ingest::RenderedPage;
use crate::testing::MockDispatch;
use crate::transport::{ChatCompletionRequest, RestRequest, Transport, TransportError};
use crate::types::{Block, PageError};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
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
}

impl ParseCtx {
    pub fn new(transport: Arc<Transport>, permits: Arc<Semaphore>) -> Self {
        Self {
            dispatcher: Dispatcher::Real(transport),
            permits,
        }
    }

    /// Build a `ParseCtx` backed by a `MockDispatch` instead of a real
    /// HTTP transport, for offline adapter tests.
    pub fn with_mock(mock: Arc<MockDispatch>, permits: Arc<Semaphore>) -> Self {
        Self {
            dispatcher: Dispatcher::Mock(mock),
            permits,
        }
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
        match &self.dispatcher {
            Dispatcher::Real(transport) => Ok(transport.dispatch(req).await?),
            Dispatcher::Mock(mock) => mock
                .dispatch(&req.endpoint)
                .ok_or_else(|| DispatchError::MockKeyMissing(req.endpoint.clone())),
        }
    }

    /// Dispatch a raw-JSON REST request (Pipeline Model Serving's
    /// contract). In `Dispatcher::Mock` mode, `endpoint` is used as the
    /// seed key — same mechanism as `dispatch`, no changes needed to
    /// `MockDispatch` itself.
    pub async fn dispatch_rest(&self, endpoint: &str, body: Value) -> Result<Value, DispatchError> {
        match &self.dispatcher {
            Dispatcher::Real(transport) => Ok(transport
                .dispatch_rest(RestRequest {
                    endpoint: endpoint.to_string(),
                    body,
                    timeout: std::time::Duration::from_secs(60),
                    max_retries: 2,
                })
                .await?),
            Dispatcher::Mock(mock) => mock
                .dispatch(endpoint)
                .ok_or_else(|| DispatchError::MockKeyMissing(endpoint.to_string())),
        }
    }

    /// Decode a page's PNG bytes and crop to `bbox_px`, returning an RGB
    /// buffer ready for `imaging::resize_by_need`/re-encoding.
    pub fn crop(&self, page: &RenderedPage, bbox_px: [i32; 4]) -> Result<image::RgbImage, String> {
        let img = image::load_from_memory(&page.png_bytes).map_err(|e| e.to_string())?;
        Ok(crate::imaging::crop(&crate::imaging::to_rgb(&img), bbox_px))
    }
}

#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    fn name(&self) -> &'static str;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum StageBackendChoice {
    Local,
    Remote,
}

/// Per-stage backend/endpoint overrides for the `pipeline` protocol
/// (T-5.1). `None` fields fall back to `PipelineAdapter::default()`'s
/// per-stage default (ARCHITECTURE.md §11.2: `table` defaults `Local`,
/// the other three default `Remote`).
#[derive(Debug, Clone, Default)]
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
        assert_eq!(blocks.len(), 1);
    }
}
