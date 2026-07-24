//! Manual smoke test for `DotsOcrAdapter` against a real vLLM endpoint.
//! Not part of the test suite (network-dependent) — run explicitly:
//!
//!   NO_PROXY=127.0.0.1,localhost cargo run --example dots_ocr_smoke -- \
//!     <page.png> <endpoint_base_url> <model_name>

use std::env;
use std::sync::Arc;
use tokio::sync::Semaphore;
use uparser_core::adapters::dots_ocr::DotsOcrAdapter;
use uparser_core::adapters::{ParseCtx, ProtocolAdapter};
use uparser_core::ingest::RenderedPage;
use uparser_core::transport::Transport;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let png_path = args
        .get(1)
        .expect("usage: dots_ocr_smoke <page.png> [endpoint] [model]");
    let endpoint_base = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:8000/v1/chat/completions".to_string());
    let model = args.get(3).cloned().unwrap_or_else(|| "model".to_string());

    let png_bytes = std::fs::read(png_path).expect("read PNG file");
    let img = image::load_from_memory(&png_bytes).expect("decode PNG");
    let (width, height) = (img.width(), img.height());

    let page = RenderedPage {
        page_num: 1,
        width,
        height,
        png_bytes,
    };

    let adapter = DotsOcrAdapter {
        endpoint_base,
        model,
        ..DotsOcrAdapter::default()
    };

    let ctx = ParseCtx::new(Arc::new(Transport::new()), Arc::new(Semaphore::new(4)));

    let started = std::time::Instant::now();
    match adapter.parse_page(&page, &ctx).await {
        Ok(blocks) => {
            eprintln!(
                "parsed {} blocks in {:?} from {}x{} page",
                blocks.len(),
                started.elapsed(),
                width,
                height
            );
            for (i, b) in blocks.iter().enumerate() {
                eprintln!(
                    "--- block {i}: category_raw={} category={:?} bbox_px={:?} reading_order={:?} error={:?}",
                    b.category_raw, b.category, b.bbox_px, b.reading_order, b.error
                );
                if let Some(t) = &b.text {
                    println!("[text] {t}");
                }
                if let Some(h) = &b.html {
                    println!("[html] {h}");
                }
                if let Some(l) = &b.latex {
                    println!("[latex] {l}");
                }
            }
        }
        Err(e) => {
            eprintln!("parse_page failed: {} (stage={:?})", e.message, e.stage);
            std::process::exit(1);
        }
    }
}
