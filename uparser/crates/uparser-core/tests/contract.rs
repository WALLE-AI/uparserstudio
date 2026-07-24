//! Contract test suite (T-9.5/T-9.6): fixed mock-model responses fed
//! through real adapters, asserting the resulting IR/Markdown is
//! consistent regardless of which protocol produced it — the kind of
//! regression a protocol-specific refactor could silently break without
//! a cross-protocol check.
//!
//! Per-adapter offline tests already covering most of T-9.5's individual
//! scope (two-stage orchestration, truncated/malformed output recovery,
//! partial-page failure isolation, capability degradation) live beside
//! each adapter in `src/adapters/*.rs` and are not duplicated here — see
//! `mineru_vlm.rs`/`dots_ocr.rs`/`monkeyocr_v2.rs`/`pipeline.rs`/
//! `paddleocr.rs`'s own `#[cfg(test)]` modules, plus `render.rs`'s
//! `insta` snapshot tests for the render layer itself (both already
//! established since P0/P1). This file adds the piece those didn't cover
//! (T-9.6): running *different* protocols against equivalent mocked
//! input and checking their outputs agree at the IR/Markdown level.

use image::{Rgb, RgbImage};
use std::sync::Arc;
use tokio::sync::Semaphore;
use uparser_core::adapters::mineru_vlm::MineruVlmAdapter;
use uparser_core::adapters::monkeyocr_v2::MonkeyOcrV2Adapter;
use uparser_core::adapters::{ParseCtx, ProtocolAdapter};
use uparser_core::ingest::RenderedPage;
use uparser_core::render;
use uparser_core::testing::MockDispatch;
use uparser_core::types::{Page, ParseResult, RoutedBy};

fn fake_page(width: u32, height: u32) -> RenderedPage {
    let img = RgbImage::from_pixel(width, height, Rgb([255, 255, 255]));
    RenderedPage {
        page_num: 1,
        width,
        height,
        png_bytes: uparser_core::imaging::to_png_bytes(&img).unwrap(),
    }
}

fn wrap_as_parse_result(protocol: &str, page: Page) -> ParseResult {
    ParseResult {
        source_path: "contract-test.pdf".into(),
        source_sha256: "n/a".into(),
        protocol: protocol.into(),
        routed_by: RoutedBy::Explicit,
        document_profile: None,
        model_endpoint: None,
        model_name: None,
        pages: vec![page],
        page_errors: vec![],
        capability_notes: vec![],
        warnings: vec![],
        timing: Default::default(),
    }
}

fn chat_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{"message": {"content": content}}]
    })
}

/// mineru-vlm and MonkeyOCRv2 are both two-stage (layout->per-block
/// recognition) protocols with different native custom-token grammars —
/// seed each with equivalent "a single text block reading 'Hello
/// contract test'" layout+content output and check the rendered
/// Markdown is byte-identical, proving `postprocess.rs`/`render/` (and
/// the adapters' own category mapping) treat structurally-equivalent
/// input from two different wire formats the same way.
#[tokio::test]
async fn mineru_vlm_and_monkeyocrv2_agree_on_markdown_for_equivalent_text_block() {
    let mineru = MineruVlmAdapter::default();
    let monkeyocr = MonkeyOcrV2Adapter::default();

    let mineru_mock = Arc::new(MockDispatch::new());
    mineru_mock.seed(
        &format!("{}#stage1", mineru.endpoint_base),
        chat_response("<|box_start|>0 0 500 200<|box_end|><|ref_start|>text<|ref_end|>"),
    );
    mineru_mock.seed(
        &format!("{}#stage2:0", mineru.endpoint_base),
        chat_response("Hello contract test"),
    );
    let mineru_ctx = ParseCtx::with_mock(mineru_mock, Arc::new(Semaphore::new(4)));
    let page = fake_page(1000, 1000);
    let mineru_blocks = mineru
        .parse_page(&page, &mineru_ctx)
        .await
        .expect("mineru-vlm parse_page succeeds");

    let monkeyocr_mock = Arc::new(MockDispatch::new());
    monkeyocr_mock.seed(
        &format!("{}#layout", monkeyocr.endpoint_base),
        chat_response("[{'bbox': [0, 0, 500, 200], 'label': 'Text'}]"),
    );
    monkeyocr_mock.seed(
        &format!("{}#recognize:0", monkeyocr.endpoint_base),
        chat_response("Hello contract test"),
    );
    let monkeyocr_ctx = ParseCtx::with_mock(monkeyocr_mock, Arc::new(Semaphore::new(4)));
    let monkeyocr_blocks = monkeyocr
        .parse_page(&page, &monkeyocr_ctx)
        .await
        .expect("monkeyocr-v2 parse_page succeeds");

    assert_eq!(mineru_blocks.len(), 1);
    assert_eq!(monkeyocr_blocks.len(), 1);
    assert_eq!(mineru_blocks[0].category.as_deref(), Some("text"));
    assert_eq!(monkeyocr_blocks[0].category.as_deref(), Some("text"));

    let mineru_page = Page {
        page_num: 1,
        width_px: page.width,
        height_px: page.height,
        blocks: mineru_blocks,
    };
    let monkeyocr_page = Page {
        page_num: 1,
        width_px: page.width,
        height_px: page.height,
        blocks: monkeyocr_blocks,
    };

    let mineru_md = render::to_markdown(&wrap_as_parse_result("mineru-vlm", mineru_page));
    let monkeyocr_md = render::to_markdown(&wrap_as_parse_result("monkeyocr-v2", monkeyocr_page));

    assert_eq!(mineru_md, monkeyocr_md);
    assert_eq!(mineru_md, "Hello contract test");
}

/// The content-list renderer must also agree on the fields that matter
/// to an Agent consumer (`category`, `text`) across the two protocols,
/// independent of each protocol's own internal `category_raw` spelling
/// (`"text"` for mineru-vlm vs. `"Text"` for MonkeyOCRv2).
#[tokio::test]
async fn content_list_categories_agree_across_protocols_despite_different_native_spelling() {
    let mineru = MineruVlmAdapter::default();
    let monkeyocr = MonkeyOcrV2Adapter::default();

    let mineru_mock = Arc::new(MockDispatch::new());
    mineru_mock.seed(
        &format!("{}#stage1", mineru.endpoint_base),
        chat_response("<|box_start|>0 0 500 200<|box_end|><|ref_start|>text<|ref_end|>"),
    );
    mineru_mock.seed(
        &format!("{}#stage2:0", mineru.endpoint_base),
        chat_response("same content"),
    );
    let mineru_ctx = ParseCtx::with_mock(mineru_mock, Arc::new(Semaphore::new(4)));
    let page = fake_page(1000, 1000);
    let mineru_blocks = mineru.parse_page(&page, &mineru_ctx).await.unwrap();

    let monkeyocr_mock = Arc::new(MockDispatch::new());
    monkeyocr_mock.seed(
        &format!("{}#layout", monkeyocr.endpoint_base),
        chat_response("[{'bbox': [0, 0, 500, 200], 'label': 'Text'}]"),
    );
    monkeyocr_mock.seed(
        &format!("{}#recognize:0", monkeyocr.endpoint_base),
        chat_response("same content"),
    );
    let monkeyocr_ctx = ParseCtx::with_mock(monkeyocr_mock, Arc::new(Semaphore::new(4)));
    let monkeyocr_blocks = monkeyocr.parse_page(&page, &monkeyocr_ctx).await.unwrap();

    let mineru_result = wrap_as_parse_result(
        "mineru-vlm",
        Page {
            page_num: 1,
            width_px: page.width,
            height_px: page.height,
            blocks: mineru_blocks,
        },
    );
    let monkeyocr_result = wrap_as_parse_result(
        "monkeyocr-v2",
        Page {
            page_num: 1,
            width_px: page.width,
            height_px: page.height,
            blocks: monkeyocr_blocks,
        },
    );

    let mineru_list: serde_json::Value =
        serde_json::from_str(&render::to_content_list(&mineru_result)).unwrap();
    let monkeyocr_list: serde_json::Value =
        serde_json::from_str(&render::to_content_list(&monkeyocr_result)).unwrap();

    assert_eq!(mineru_list[0]["category"], monkeyocr_list[0]["category"]);
    assert_eq!(mineru_list[0]["text"], monkeyocr_list[0]["text"]);
}
