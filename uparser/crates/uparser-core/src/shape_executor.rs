//! Shared execution mechanics for model-protocol shapes.
//!
//! Protocol adapters retain preprocessing, prompts, decoding and repair rules.
//! This module owns only transport-stage error boundaries and deterministic
//! collection of concurrently recognized regions.

use crate::adapters::ParseCtx;
use crate::ingest::RenderedPage;
use crate::transport::ChatCompletionRequest;
use crate::types::PageError;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::time::Duration;

pub async fn chat_stage(
    page: &RenderedPage,
    ctx: &ParseCtx,
    request: ChatCompletionRequest,
    stage: &'static str,
) -> Result<Value, PageError> {
    ctx.dispatch(request).await.map_err(|error| PageError {
        page_num: page.page_num,
        message: error.to_string(),
        stage: Some(stage.to_owned()),
    })
}

pub async fn rest_stage(
    page: &RenderedPage,
    ctx: &ParseCtx,
    endpoint: &str,
    request: Value,
    timeout: Duration,
    max_retries: u32,
    stage: &'static str,
) -> Result<Value, PageError> {
    ctx.dispatch_rest(endpoint, request, timeout, max_retries)
        .await
        .map_err(|error| PageError {
            page_num: page.page_num,
            message: error.to_string(),
            stage: Some(stage.to_owned()),
        })
}

pub async fn collect_indexed<K, V, E, F>(
    futures: impl IntoIterator<Item = F>,
) -> HashMap<K, Result<Option<V>, E>>
where
    K: Eq + Hash,
    F: Future<Output = (K, Result<Option<V>, E>)>,
{
    futures::future::join_all(futures)
        .await
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn indexed_collection_preserves_keys_across_concurrent_completion() {
        let results = collect_indexed([
            std::future::ready((2usize, Ok::<_, String>(Some("third")))),
            std::future::ready((0usize, Ok::<_, String>(Some("first")))),
            std::future::ready((1usize, Ok::<_, String>(None))),
        ])
        .await;

        assert_eq!(results[&0], Ok(Some("first")));
        assert_eq!(results[&1], Ok(None));
        assert_eq!(results[&2], Ok(Some("third")));
    }
}
