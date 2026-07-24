//! Node.js bindings (T-10.1), via napi-rs. Thin wrapper over
//! `uparser_core::api` — `parse`/`classify` return the exact same JSON
//! text `serde_json::to_string` would produce for the CLI's `--format
//! json` output, so Node/CLI/Python all agree on IR shape by
//! construction rather than by parallel maintenance.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use uparser_core::api::{self, ParseOptions};

#[napi(object)]
pub struct JsParseOptions {
    pub protocol: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub window_size: Option<u32>,
    pub max_concurrency: Option<u32>,
    pub no_cache: Option<bool>,
}

fn to_parse_options(options: Option<JsParseOptions>) -> ParseOptions {
    let mut opts = ParseOptions::default();
    let Some(o) = options else { return opts };
    if let Some(protocol) = o.protocol {
        opts.protocol = protocol;
    }
    opts.endpoint = o.endpoint;
    opts.model = o.model;
    if let Some(w) = o.window_size {
        opts.window_size = w as usize;
    }
    if let Some(c) = o.max_concurrency {
        opts.max_concurrency = c as usize;
    }
    if let Some(nc) = o.no_cache {
        opts.no_cache = nc;
    }
    opts
}

/// Parse a document, returning the `ParseResult` serialized as a JSON
/// string.
#[napi]
pub async fn parse(path: String, options: Option<JsParseOptions>) -> Result<String> {
    let opts = to_parse_options(options);
    let result = api::parse(&path, &opts)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(serde_json::to_string(&result).expect("ParseResult is always serializable"))
}

/// Run the Profiler only, returning the `DocumentProfile` serialized as
/// a JSON string.
#[napi]
pub async fn classify(path: String) -> Result<String> {
    let profile = api::classify(&path)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(serde_json::to_string(&profile).expect("DocumentProfile is always serializable"))
}
