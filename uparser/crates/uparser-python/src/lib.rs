//! Python bindings (T-10.2), via PyO3/maturin. Thin wrapper over
//! `uparser_core::api` — `parse`/`classify` return the exact same JSON
//! text `serde_json::to_string` would produce for the CLI's `--format
//! json` output, so Python/CLI/Node all agree on IR shape by
//! construction rather than by parallel maintenance.
//!
//! PyO3 has no first-class async-fn-in-Python-module story as simple as
//! napi-rs's `#[napi] async fn` (it needs `pyo3-async-runtimes` plus an
//! asyncio event loop bridge) — for this pass, `parse`/`classify` block
//! on a lazily-initialized Tokio runtime instead, the same
//! `Runtime::block_on` pattern `cli.rs` itself already uses for every
//! command. A real asyncio-native API is a documented future
//! enhancement, not required for T-10.2's "same core, same IR" claim.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::sync::OnceLock;
use uparser_core::api::{self, ParseOptions};

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Runtime::new().expect("failed to create Tokio runtime for PyO3 bindings")
    })
}

#[pyfunction]
#[pyo3(signature = (
    path,
    protocol=None,
    endpoint=None,
    model=None,
    window_size=None,
    max_concurrency=None,
    no_cache=None
))]
#[allow(clippy::too_many_arguments)]
fn parse(
    path: String,
    protocol: Option<String>,
    endpoint: Option<String>,
    model: Option<String>,
    window_size: Option<usize>,
    max_concurrency: Option<usize>,
    no_cache: Option<bool>,
) -> PyResult<String> {
    let mut options = ParseOptions::default();
    if let Some(protocol) = protocol {
        options.protocol = protocol;
    }
    options.endpoint = endpoint;
    options.model = model;
    if let Some(w) = window_size {
        options.window_size = w;
    }
    if let Some(c) = max_concurrency {
        options.max_concurrency = c;
    }
    if let Some(nc) = no_cache {
        options.no_cache = nc;
    }

    let result = runtime()
        .block_on(api::parse(&path, &options))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(serde_json::to_string(&result).expect("ParseResult is always serializable"))
}

#[pyfunction]
fn classify(path: String) -> PyResult<String> {
    let profile = runtime()
        .block_on(api::classify(&path))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(serde_json::to_string(&profile).expect("DocumentProfile is always serializable"))
}

#[pymodule]
fn _uparser(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse, m)?)?;
    m.add_function(wrap_pyfunction!(classify, m)?)?;
    Ok(())
}
