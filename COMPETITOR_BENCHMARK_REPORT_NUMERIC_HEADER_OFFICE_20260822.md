# UParser Competitor Benchmark Report

> Generated: `2026-08-22T14:47:26.546342+00:00`  
> Overall gate: **FAIL**

## Gate Summary

| Suite | Competitor | Performance/reliability | Quality | Overall |
|---|---|---:|---:|---:|
| office | anydoc | FAIL | INSUFFICIENT | FAIL |

## Methodology

- CLI end-to-end timing includes process startup and output emission.
- Output channels match the paired competitor: stdout for PDF comparisons and in-process file output for the Office comparison.
- Runs use randomized Latin rotation; no startup subtraction or best-of-N selection.

## Measurements

### office

| Engine | Success | Median ms | P95 ms | Docs/s | Peak RSS bytes |
|---|---:|---:|---:|---:|---:|
| anydoc | 1.000 | 13.996 | 16.662 | 70.3081 | 5775360 |
| uparser-native | 1.000 | 15.695 | 18.691 | 62.3963 | 6701056 |

## Gate Details

### office / anydoc

- Performance failures: median, median_10pct, p95, throughput, rss, elapsed_ratio_ci_upper_below_one, throughput_ratio_ci_lower_above_one
- Quality: paired semantic evaluations were not provided

## Interpretation

`FAIL` and `INSUFFICIENT` both block a comprehensive-leading claim. Quality gates use paired per-document deltas and bootstrap confidence intervals; missing semantic evaluations are never inferred from non-empty output.
