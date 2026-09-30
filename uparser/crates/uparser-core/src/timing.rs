//! Per-phase wall-clock timings for one run (B.1 in
//! `ARCHITECTURE_V2_REMEDIATION_PLAN.md`).
//!
//! `ParseResult.timing` has existed since P0 but was filled with
//! `Default::default()` on every code path, so a slow run could only be
//! diagnosed with an external stopwatch. The phases recorded here are the
//! boundaries the runner already has — no new stage was introduced to make
//! the numbers look complete.
//!
//! # Honesty rules this module exists to enforce
//!
//! * **Keys are only present when that phase actually ran.** A cache hit has
//!   no [`MODEL`] entry rather than a `0.0` one, because "the model was never
//!   called" and "the model took no measurable time" are different facts.
//! * **[`TOTAL`] is measured, not summed.** It is the runner's own end-to-end
//!   wall clock, so it includes whatever falls between the recorded phases
//!   (allocation, cancellation checks, cheap bookkeeping). `TOTAL` is
//!   therefore `>=` the sum of the phases; a large gap is itself a signal.
//! * **Render time is not in here.** `ParseResult.timing` is serialized *by*
//!   the renderer, so a `render_ms` key inside it could never include its own
//!   cost. The CLI measures rendering separately and reports it through
//!   `--stats` (stderr), which is also the only way a `--format markdown` run
//!   can see any of these numbers at all.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// Format detection plus document analysis: the native PDF engine's
/// `process_pdf_mem`, or the document engine's structured parse. For `native`
/// this is usually the dominant phase, because [`MODEL`] then reuses the
/// artifact analysis already produced.
pub const ANALYZE: &str = "analyze_ms";
/// Semantic enrichment (`auto` only), routing and preprocess planning.
pub const PLAN: &str = "plan_ms";
/// Content-hash cache lookup. Recorded even on a miss — a slow lookup is
/// worth seeing.
pub const CACHE_LOOKUP: &str = "cache_lookup_ms";
/// Page-source materialization: LibreOffice conversion for office inputs,
/// opening the PDF, decoding a single-image input.
///
/// **Not** the whole of rasterization: `pdf_page_source` renders pages on
/// demand, so per-page raster cost is inside [`MODEL`] where it actually
/// happens, interleaved with dispatch.
pub const INGEST: &str = "ingest_ms";
/// Protocol execution: the scheduler's per-page adapter dispatch (including
/// the on-demand page rasterization it drives), or the native lane's document
/// build from the analysis artifact. Absent when the cache served the run.
pub const MODEL: &str = "model_ms";
/// Paragraph merge + content normalization.
pub const POSTPROCESS: &str = "postprocess_ms";
/// Image-asset materialization and writes.
pub const ASSETS: &str = "assets_ms";
/// Cache write.
pub const CACHE_WRITE: &str = "cache_write_ms";
/// The runner's end-to-end wall clock (measured, see the module doc).
pub const TOTAL: &str = "total_ms";
/// Output rendering. Never present in `ParseResult.timing`; recorded by the
/// CLI for its `--stats` line only.
pub const RENDER: &str = "render_ms";

/// Phase name → milliseconds. Ordered so a `--stats` line and a JSON object
/// both come out deterministically.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhaseTimings {
    entries: BTreeMap<String, f64>,
}

impl PhaseTimings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild a collector from a `ParseResult.timing` map, so a caller that
    /// only has the serialized form can keep appending to it.
    pub fn from_map(map: &HashMap<String, f64>) -> Self {
        Self {
            entries: map.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        }
    }

    /// Add `elapsed` to `key`, accumulating if the phase runs more than once
    /// (postprocess runs per streamed window as well as on the aggregate).
    pub fn record(&mut self, key: &str, elapsed: Duration) {
        *self.entries.entry(key.to_owned()).or_insert(0.0) += elapsed.as_secs_f64() * 1000.0;
    }

    /// Time `body` and record it under `key`.
    pub fn measure<T>(&mut self, key: &str, body: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = body();
        self.record(key, start.elapsed());
        value
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        self.entries.get(key).copied()
    }

    /// Fold another collector in, accumulating shared keys.
    pub fn merge(&mut self, other: &PhaseTimings) {
        for (key, value) in &other.entries {
            *self.entries.entry(key.clone()).or_insert(0.0) += value;
        }
    }

    /// Sum of every phase except [`TOTAL`], which is measured separately and
    /// would otherwise be double counted.
    pub fn phase_sum(&self) -> f64 {
        self.entries
            .iter()
            .filter(|(key, _)| key.as_str() != TOTAL)
            .map(|(_, value)| value)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `"analyze=12.3ms model=980.1ms total=1002.7ms"` — the `--stats` line.
    pub fn summary_line(&self) -> String {
        self.entries
            .iter()
            .map(|(key, value)| {
                let name = key.strip_suffix("_ms").unwrap_or(key.as_str());
                format!("{name}={value:.1}ms")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The shape `ParseResult.timing` stores.
    pub fn to_map(&self) -> HashMap<String, f64> {
        self.entries
            .iter()
            .map(|(key, value)| (key.clone(), *value))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_that_never_ran_has_no_key_at_all() {
        let mut timings = PhaseTimings::new();
        timings.record(ANALYZE, Duration::from_millis(5));
        let map = timings.to_map();
        assert!(map.contains_key(ANALYZE));
        // Not `Some(0.0)` — see the module doc's first honesty rule.
        assert!(!map.contains_key(MODEL));
    }

    #[test]
    fn repeated_records_accumulate_instead_of_overwriting() {
        let mut timings = PhaseTimings::new();
        timings.record(POSTPROCESS, Duration::from_millis(10));
        timings.record(POSTPROCESS, Duration::from_millis(5));
        assert!((timings.get(POSTPROCESS).unwrap() - 15.0).abs() < 0.001);
    }

    #[test]
    fn merge_accumulates_shared_keys_and_adopts_new_ones() {
        let mut left = PhaseTimings::new();
        left.record(ANALYZE, Duration::from_millis(4));
        let mut right = PhaseTimings::new();
        right.record(ANALYZE, Duration::from_millis(6));
        right.record(MODEL, Duration::from_millis(1));
        left.merge(&right);
        assert!((left.get(ANALYZE).unwrap() - 10.0).abs() < 0.001);
        assert!((left.get(MODEL).unwrap() - 1.0).abs() < 0.001);
    }

    #[test]
    fn phase_sum_excludes_the_measured_total() {
        let mut timings = PhaseTimings::new();
        timings.record(ANALYZE, Duration::from_millis(3));
        timings.record(MODEL, Duration::from_millis(7));
        timings.record(TOTAL, Duration::from_millis(50));
        assert!((timings.phase_sum() - 10.0).abs() < 0.001);
    }

    #[test]
    fn measure_records_the_body_and_returns_its_value() {
        let mut timings = PhaseTimings::new();
        let value = timings.measure(INGEST, || {
            std::thread::sleep(Duration::from_millis(2));
            41 + 1
        });
        assert_eq!(value, 42);
        assert!(timings.get(INGEST).unwrap() >= 1.0);
    }

    #[test]
    fn summary_line_drops_the_unit_suffix_and_is_ordered() {
        let mut timings = PhaseTimings::new();
        timings.record(TOTAL, Duration::from_millis(2));
        timings.record(ANALYZE, Duration::from_millis(1));
        assert_eq!(timings.summary_line(), "analyze=1.0ms total=2.0ms");
    }
}
