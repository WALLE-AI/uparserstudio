//! Document-level scheduling: processing-window batching, a shared
//! cross-page concurrency budget, and per-page failure isolation.
//! Per ARCHITECTURE.md §2.2 / T-0.5.

use crate::adapters::{ParseCtx, ProtocolAdapter};
use crate::ingest::RenderedPage;
use crate::transport::Transport;
use crate::types::{Page, PageError};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// A page's adapter task panicked (or was cancelled) — isolate it into a
/// `PageError` instead of letting the panic propagate out of `run`/
/// `run_streaming` and abort every other in-flight/already-completed
/// page in the same call. Adapters process untrusted, possibly malformed
/// model output; a single indexing bug reacting to one page's response
/// should not discard every other page's already-successful result —
/// this was previously not the case (`handle.await.expect(...)`), a real
/// gap distinct from the page-level `Result::Err` isolation this module
/// already had.
fn page_panic_error(page_num: u32, join_err: &tokio::task::JoinError) -> PageError {
    PageError {
        page_num,
        message: format!("adapter task failed to complete: {join_err}"),
        stage: Some("scheduler".into()),
    }
}

/// Fired once per completed page (success or failure) via
/// `Scheduler::run_with_progress` — lets a caller report progress on a
/// long-running document without waiting for the whole `run` call to
/// return. Motivated by a real diagnosis gap: this session's scheduler
/// deadlock bug produced zero signal of any kind for 20+ minutes before
/// being found, because nothing surfaced "no page has completed
/// recently" — a caller watching this channel can build exactly that
/// kind of stall detection (see `cli.rs`'s watchdog).
#[derive(Debug, Clone)]
pub struct PageProgress {
    pub page_num: u32,
    pub ok: bool,
    pub completed: usize,
    pub total: usize,
}

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
    ///
    /// `permits` bounds concurrent *network dispatches*, not concurrent
    /// pages — it must not be acquired here for the whole
    /// `parse_page()` call. Adapters that fan out per-block requests
    /// within a page (`mineru-vlm`/`pipeline`/`monkeyocr-v2`) acquire
    /// their own permit per block via `ctx.acquire_permit()`; wrapping
    /// the entire page in an outer permit here as well previously
    /// deadlocked as soon as `window_size` concurrently-running pages
    /// exhausted every permit before any of them reached their own
    /// inner per-block acquisition — confirmed live against a real
    /// multi-page document (7 pages, default `max_concurrency=4`): the
    /// process hung indefinitely with zero CPU/network activity, since
    /// every in-flight page was parked waiting on a permit held by
    /// another page that was itself parked the same way.
    ///
    /// Also returns every warning recorded via `ctx.warn()` across all
    /// pages (T-9-era gap: previously only ever reached a bare
    /// `eprintln!`, with no channel into `ParseResult.warnings` for
    /// callers not watching stderr — e.g. the `api.rs`/binding-layer
    /// path).
    pub async fn run(
        &self,
        adapter: Arc<dyn ProtocolAdapter>,
        transport: Arc<Transport>,
        permits: Arc<Semaphore>,
        pages: Vec<RenderedPage>,
    ) -> (Vec<Page>, Vec<PageError>, Vec<String>) {
        self.run_with_progress(adapter, transport, permits, pages, |_| {})
            .await
    }

    /// Same as `run`, but invokes `on_page` once per completed page
    /// (success or failure) — the finer, per-page-not-per-window
    /// progress channel `run_streaming`'s `on_window` doesn't provide.
    /// `run` itself is just this with a no-op callback.
    pub async fn run_with_progress<F>(
        &self,
        adapter: Arc<dyn ProtocolAdapter>,
        transport: Arc<Transport>,
        permits: Arc<Semaphore>,
        pages: Vec<RenderedPage>,
        mut on_page: F,
    ) -> (Vec<Page>, Vec<PageError>, Vec<String>)
    where
        F: FnMut(&PageProgress),
    {
        let total = pages.len();
        let mut completed = 0usize;
        let mut out_pages = Vec::new();
        let mut out_errors = Vec::new();
        let warnings = Arc::new(Mutex::new(Vec::new()));

        for window in pages.chunks(self.window_size.max(1)) {
            let mut handles = Vec::with_capacity(window.len());
            for page in window {
                let page_num = page.page_num;
                let page = page.clone();
                let adapter = Arc::clone(&adapter);
                let ctx = ParseCtx::new_with_shared_warnings(
                    Arc::clone(&transport),
                    Arc::clone(&permits),
                    Arc::clone(&warnings),
                );
                handles.push((
                    page_num,
                    tokio::spawn(async move {
                        let result = adapter.parse_page(&page, &ctx).await;
                        (page, result)
                    }),
                ));
            }
            for (page_num, handle) in handles {
                let ok = match handle.await {
                    Ok((page, Ok(blocks))) => {
                        out_pages.push(Page {
                            page_num: page.page_num,
                            width_px: page.width,
                            height_px: page.height,
                            blocks,
                        });
                        true
                    }
                    Ok((_, Err(err))) => {
                        out_errors.push(err);
                        false
                    }
                    Err(join_err) => {
                        out_errors.push(page_panic_error(page_num, &join_err));
                        false
                    }
                };
                completed += 1;
                on_page(&PageProgress {
                    page_num,
                    ok,
                    completed,
                    total,
                });
            }
            // `window`'s `RenderedPage`s (and their PNG buffers) are
            // dropped here, before the next window starts.
        }

        out_pages.sort_by_key(|p| p.page_num);
        let warnings = warnings
            .lock()
            .expect("warnings mutex not poisoned")
            .clone();
        (out_pages, out_errors, warnings)
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
    ) -> (Vec<Page>, Vec<PageError>, Vec<String>)
    where
        F: FnMut(&[Page], &[PageError], &[String]),
    {
        let mut out_pages = Vec::new();
        let mut out_errors = Vec::new();
        let mut out_warnings = Vec::new();

        for window in pages.chunks(self.window_size.max(1)) {
            let window_warnings = Arc::new(Mutex::new(Vec::new()));
            let mut handles = Vec::with_capacity(window.len());
            for page in window {
                let page_num = page.page_num;
                let page = page.clone();
                let adapter = Arc::clone(&adapter);
                let ctx = ParseCtx::new_with_shared_warnings(
                    Arc::clone(&transport),
                    Arc::clone(&permits),
                    Arc::clone(&window_warnings),
                );
                handles.push((
                    page_num,
                    tokio::spawn(async move {
                        let result = adapter.parse_page(&page, &ctx).await;
                        (page, result)
                    }),
                ));
            }

            let mut window_pages = Vec::with_capacity(window.len());
            let mut window_errors = Vec::new();
            for (page_num, handle) in handles {
                match handle.await {
                    Ok((page, Ok(blocks))) => window_pages.push(Page {
                        page_num: page.page_num,
                        width_px: page.width,
                        height_px: page.height,
                        blocks,
                    }),
                    Ok((_, Err(err))) => window_errors.push(err),
                    Err(join_err) => window_errors.push(page_panic_error(page_num, &join_err)),
                }
            }
            window_pages.sort_by_key(|p| p.page_num);
            let window_warnings = window_warnings
                .lock()
                .expect("warnings mutex not poisoned")
                .clone();
            on_window(&window_pages, &window_errors, &window_warnings);

            out_pages.extend(window_pages);
            out_errors.extend(window_errors);
            out_warnings.extend(window_warnings);
        }

        out_pages.sort_by_key(|p| p.page_num);
        (out_pages, out_errors, out_warnings)
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
        let (pages, errors, _warnings) = scheduler
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

    /// An adapter that acquires its own permit (matching real adapters'
    /// pattern — see `mineru_vlm.rs`'s per-block loop, not the scheduler)
    /// and holds it for a short sleep, letting the test observe how many
    /// permit-holding sections are in flight at once. The scheduler
    /// itself no longer acquires permits (see `run`'s doc comment on the
    /// deadlock that caused).
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
            ctx: &ParseCtx,
        ) -> Result<Vec<Block>, PageError> {
            let _permit = ctx.acquire_permit().await;
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
                asset_bytes: None,
                asset_path: None,
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
        let (pages, errors, _warnings) = scheduler
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

    /// An adapter shaped like the real two-stage adapters
    /// (`mineru-vlm`/`pipeline`/`monkeyocr-v2`): each page fans out to
    /// several "blocks", and *each block* acquires its own permit from
    /// the same document-level semaphore the scheduler is given —
    /// exactly the nesting that deadlocked when `scheduler.rs::run`
    /// also held an outer permit for the whole page. Confirmed live
    /// against a real 7-page PDF through `mineru-vlm` before the fix
    /// (default `--max-concurrency 4`): the process hung indefinitely
    /// with zero CPU/network activity once 4 pages were concurrently
    /// in-flight, since all 4 permits were held by page-level guards
    /// none of the pages' own block-level `acquire_permit()` calls
    /// could ever obtain.
    struct NestedPermitAdapter {
        blocks_per_page: usize,
    }

    #[async_trait]
    impl ProtocolAdapter for NestedPermitAdapter {
        fn name(&self) -> &'static str {
            "nested-permit"
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
            ctx: &ParseCtx,
        ) -> Result<Vec<Block>, PageError> {
            for _ in 0..self.blocks_per_page {
                let _permit = ctx.acquire_permit().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn many_concurrent_pages_with_per_block_permits_does_not_deadlock() {
        let adapter: Arc<dyn ProtocolAdapter> =
            Arc::new(NestedPermitAdapter { blocks_per_page: 5 });
        // More pages than permits, and window_size lets them all run
        // concurrently — the exact shape that deadlocked before the fix.
        let scheduler = Scheduler::new(20);

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            scheduler.run(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(4)),
                fake_pages(20),
            ),
        )
        .await
        .expect("scheduler.run must not deadlock when pages > permits");

        assert_eq!(result.0.len(), 20);
        assert!(result.1.is_empty());
    }

    /// An adapter that panics on one specific page and succeeds
    /// normally on every other — used to prove a single page's panic is
    /// isolated into that page's `PageError` rather than aborting the
    /// whole `run` call and discarding every other page's already-
    /// completed result (the previous behavior: `handle.await.expect(
    /// "adapter task panicked")` re-panicked the whole scheduler).
    struct PanicOnPageAdapter {
        panic_on_page: u32,
    }

    #[async_trait]
    impl ProtocolAdapter for PanicOnPageAdapter {
        fn name(&self) -> &'static str {
            "panic-on-page"
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
            page: &RenderedPage,
            _ctx: &ParseCtx,
        ) -> Result<Vec<Block>, PageError> {
            if page.page_num == self.panic_on_page {
                panic!("simulated adapter bug on page {}", page.page_num);
            }
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn panic_on_one_page_is_isolated_not_fatal_to_the_whole_run() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(PanicOnPageAdapter { panic_on_page: 3 });
        let scheduler = Scheduler::new(16);

        let (pages, errors, _warnings) = scheduler
            .run(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(4)),
                fake_pages(5),
            )
            .await;

        // The other 4 pages must still have completed successfully —
        // previously, page 3's panic would have re-panicked the whole
        // `run` call via `handle.await.expect(...)`, losing all 5.
        assert_eq!(pages.len(), 4);
        assert!(pages.iter().all(|p| p.page_num != 3));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].page_num, 3);
        assert!(errors[0].message.contains("panicked"));
    }

    /// An adapter that records a `ctx.warn()` per page — used to prove
    /// `run`/`run_streaming` genuinely collect warnings across every
    /// page into one document-level `Vec<String>` (T-9-era gap: this
    /// channel used to only ever reach a bare `eprintln!`, with no path
    /// into `ParseResult.warnings` for callers not watching stderr).
    struct WarningAdapter;

    #[async_trait]
    impl ProtocolAdapter for WarningAdapter {
        fn name(&self) -> &'static str {
            "warning-adapter"
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
            page: &RenderedPage,
            ctx: &ParseCtx,
        ) -> Result<Vec<Block>, PageError> {
            ctx.warn(format!("synthetic warning for page {}", page.page_num));
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn run_collects_warnings_from_every_page() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(WarningAdapter);
        let scheduler = Scheduler::new(16);
        let (_pages, _errors, warnings) = scheduler
            .run(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(4)),
                fake_pages(3),
            )
            .await;

        assert_eq!(warnings.len(), 3);
        for i in 1..=3 {
            assert!(
                warnings.contains(&format!("synthetic warning for page {i}")),
                "missing warning for page {i} in {warnings:?}"
            );
        }
    }

    #[tokio::test]
    async fn run_with_progress_invokes_callback_once_per_page_with_monotonic_completed_count() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(MockAdapter { fail_on_page: None });
        let scheduler = Scheduler::new(16);
        let progress_events = Arc::new(Mutex::new(Vec::new()));
        let progress_events_clone = Arc::clone(&progress_events);

        let (pages, errors, _warnings) = scheduler
            .run_with_progress(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(4)),
                fake_pages(5),
                move |event| {
                    progress_events_clone
                        .lock()
                        .expect("not poisoned")
                        .push(event.clone());
                },
            )
            .await;

        assert_eq!(pages.len(), 5);
        assert!(errors.is_empty());

        let events = progress_events.lock().expect("not poisoned").clone();
        assert_eq!(events.len(), 5, "expected one callback per page");
        assert!(events.iter().all(|e| e.ok));
        assert!(events.iter().all(|e| e.total == 5));
        // `completed` must be a 1..=5 permutation (monotonic per the
        // order pages finish, not necessarily page_num order under
        // concurrency) — every value 1..=5 appears exactly once.
        let mut completed: Vec<usize> = events.iter().map(|e| e.completed).collect();
        completed.sort_unstable();
        assert_eq!(completed, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn run_streaming_invokes_callback_once_per_window_and_matches_run_aggregate() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(MockAdapter { fail_on_page: None });
        let scheduler = Scheduler::new(4);
        let window_sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let window_sizes_clone = Arc::clone(&window_sizes);

        let (pages, errors, _warnings) = scheduler
            .run_streaming(
                adapter,
                Arc::new(Transport::new()),
                Arc::new(Semaphore::new(8)),
                fake_pages(10),
                move |window_pages, _window_errors, _window_warnings| {
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
