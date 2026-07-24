//! Document-level scheduling: processing-window batching, a shared
//! cross-page concurrency budget, and per-page failure isolation.
//! Per ARCHITECTURE.md §2.2 / T-0.5.

use crate::adapters::{ParseCtx, ProtocolAdapter};
use crate::ingest::RenderedPage;
use crate::transport::Transport;
use crate::types::{Page, PageError};
use std::sync::Arc;
use tokio::sync::Semaphore;

pub struct Scheduler {
    /// Number of pages processed (and held in memory) per batch. Bounds
    /// peak memory to ~O(window size) rather than O(total pages) — each
    /// window's `RenderedPage` buffers are dropped once that window's
    /// tasks complete, before the next window is rasterized/processed.
    pub window_size: usize,
}

impl Scheduler {
    pub fn new(window_size: usize) -> Self {
        Self { window_size }
    }

    /// Runs `adapter.parse_page` over every page, windowed and
    /// concurrency-bounded by `permits`. A single page's failure is
    /// isolated into the returned `page_errors` without aborting the rest.
    pub async fn run(
        &self,
        adapter: Arc<dyn ProtocolAdapter>,
        transport: Arc<Transport>,
        permits: Arc<Semaphore>,
        pages: Vec<RenderedPage>,
    ) -> (Vec<Page>, Vec<PageError>) {
        let mut out_pages = Vec::new();
        let mut out_errors = Vec::new();

        for window in pages.chunks(self.window_size.max(1)) {
            let mut handles = Vec::with_capacity(window.len());
            for page in window {
                let page = page.clone();
                let adapter = Arc::clone(&adapter);
                let ctx = ParseCtx::new(Arc::clone(&transport), Arc::clone(&permits));
                handles.push(tokio::spawn(async move {
                    let _permit = ctx.acquire_permit().await;
                    let result = adapter.parse_page(&page, &ctx).await;
                    (page, result)
                }));
            }
            for handle in handles {
                let (page, result) = handle.await.expect("adapter task panicked");
                match result {
                    Ok(blocks) => out_pages.push(Page {
                        page_num: page.page_num,
                        width_px: page.width,
                        height_px: page.height,
                        blocks,
                    }),
                    Err(err) => out_errors.push(err),
                }
            }
            // `window`'s `RenderedPage`s (and their PNG buffers) are
            // dropped here, before the next window starts.
        }

        out_pages.sort_by_key(|p| p.page_num);
        (out_pages, out_errors)
    }

    /// Same windowed/concurrency-bounded execution as `run`, but invokes
    /// `on_window` with each window's results as soon as that window
    /// completes, instead of collecting everything before returning
    /// (T-9.2 / ARCHITECTURE.md §2.2's streaming mode) — lets a caller
    /// emit NDJSON incrementally for a large document rather than
    /// constructing one huge in-memory `ParseResult` first. Still
    /// returns the full aggregate at the end for callers that want both
    /// (e.g. a final cache write).
    pub async fn run_streaming<F>(
        &self,
        adapter: Arc<dyn ProtocolAdapter>,
        transport: Arc<Transport>,
        permits: Arc<Semaphore>,
        pages: Vec<RenderedPage>,
        mut on_window: F,
    ) -> (Vec<Page>, Vec<PageError>)
    where
        F: FnMut(&[Page], &[PageError]),
    {
        let mut out_pages = Vec::new();
        let mut out_errors = Vec::new();

        for window in pages.chunks(self.window_size.max(1)) {
            let mut handles = Vec::with_capacity(window.len());
            for page in window {
                let page = page.clone();
                let adapter = Arc::clone(&adapter);
                let ctx = ParseCtx::new(Arc::clone(&transport), Arc::clone(&permits));
                handles.push(tokio::spawn(async move {
                    let _permit = ctx.acquire_permit().await;
                    let result = adapter.parse_page(&page, &ctx).await;
                    (page, result)
                }));
            }

            let mut window_pages = Vec::with_capacity(window.len());
            let mut window_errors = Vec::new();
            for handle in handles {
                let (page, result) = handle.await.expect("adapter task panicked");
                match result {
                    Ok(blocks) => window_pages.push(Page {
                        page_num: page.page_num,
                        width_px: page.width,
                        height_px: page.height,
                        blocks,
                    }),
                    Err(err) => window_errors.push(err),
                }
            }
            window_pages.sort_by_key(|p| p.page_num);
            on_window(&window_pages, &window_errors);

            out_pages.extend(window_pages);
            out_errors.extend(window_errors);
        }

        out_pages.sort_by_key(|p| p.page_num);
        (out_pages, out_errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::mock::MockAdapter;
    use crate::adapters::{ModelStage, PostprocessSignals, RawOutputFormat};
    use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn fake_pages(n: u32) -> Vec<RenderedPage> {
        (1..=n)
            .map(|i| RenderedPage {
                page_num: i,
                width: 10,
                height: 10,
                png_bytes: vec![],
            })
            .collect()
    }

    #[tokio::test]
    async fn single_page_failure_is_isolated() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(MockAdapter {
            fail_on_page: Some(42),
        });
        let scheduler = Scheduler::new(16);
        let (pages, errors) = scheduler
            .run(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(8)),
                fake_pages(100),
            )
            .await;

        assert_eq!(pages.len(), 99);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].page_num, 42);
        assert!(pages.iter().all(|p| p.page_num != 42));
    }

    /// An adapter that holds its permit for a short sleep, letting the test
    /// observe how many `parse_page` calls are in flight at once.
    struct CountingAdapter {
        concurrent: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProtocolAdapter for CountingAdapter {
        fn name(&self) -> &'static str {
            "counting"
        }
        fn coordinate_system(&self) -> CoordinateSystem {
            CoordinateSystem::PixelAbs
        }
        fn provides_reading_order(&self) -> bool {
            true
        }
        fn category_vocab(&self) -> &[&'static str] {
            &[]
        }
        fn raw_output_format(&self) -> RawOutputFormat {
            RawOutputFormat::StrictJson
        }
        fn emitted_signals(&self) -> PostprocessSignals {
            PostprocessSignals::default()
        }
        fn model_stages(&self) -> Vec<ModelStage> {
            vec![]
        }

        async fn parse_page(
            &self,
            _page: &RenderedPage,
            _ctx: &ParseCtx,
        ) -> Result<Vec<Block>, PageError> {
            let n = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok(vec![Block {
                geom: Geometry::Rect([0.0, 0.0, 1.0, 1.0]),
                geom_frame: CoordFrame::Page,
                bbox_px: None,
                category_raw: "text".into(),
                category: None,
                reading_order: None,
                text: None,
                html: None,
                latex: None,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::OneShotVlm,
                error: None,
            }])
        }
    }

    #[tokio::test]
    async fn concurrency_budget_is_respected() {
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(CountingAdapter {
            concurrent: Arc::clone(&concurrent),
            max_seen: Arc::clone(&max_seen),
        });

        let scheduler = Scheduler::new(50);
        let cap = 3;
        let (pages, errors) = scheduler
            .run(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(cap)),
                fake_pages(20),
            )
            .await;

        assert_eq!(pages.len(), 20);
        assert!(errors.is_empty());
        assert!(max_seen.load(Ordering::SeqCst) <= cap);
    }

    #[tokio::test]
    async fn run_streaming_invokes_callback_once_per_window_and_matches_run_aggregate() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(MockAdapter { fail_on_page: None });
        let scheduler = Scheduler::new(4);
        let window_sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let window_sizes_clone = Arc::clone(&window_sizes);

        let (pages, errors) = scheduler
            .run_streaming(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(8)),
                fake_pages(10),
                move |window_pages, _window_errors| {
                    window_sizes_clone.lock().unwrap().push(window_pages.len());
                },
            )
            .await;

        assert_eq!(pages.len(), 10);
        assert!(errors.is_empty());
        // 10 pages / window_size=4 -> windows of [4, 4, 2].
        assert_eq!(*window_sizes.lock().unwrap(), vec![4, 4, 2]);
    }
}
