# UParser Competitor Benchmark Report

> Generated: `2026-08-22T14:48:01.753576+00:00`  
> Overall gate: **FAIL**

## Gate Summary

| Suite | Competitor | Performance/reliability | Quality | Overall |
|---|---|---:|---:|---:|
| pdf | pdf-inspector | FAIL | PASS | FAIL |
| pdf | liteparse-text | FAIL | FAIL | FAIL |
| office | anydoc | FAIL | INSUFFICIENT | FAIL |

## Methodology

- CLI end-to-end timing includes process startup and output emission.
- Output channels match the paired competitor: stdout for PDF comparisons and in-process file output for the Office comparison.
- Runs use randomized Latin rotation; no startup subtraction or best-of-N selection.

## Measurements

### pdf

| Engine | Success | Median ms | P95 ms | Docs/s | Peak RSS bytes |
|---|---:|---:|---:|---:|---:|
| pdf-inspector | 0.995 | 34.864 | 72.439 | 24.4583 | 15458304 |
| liteparse-text | 1.000 | 43.767 | 57.549 | 21.8758 | 28766208 |
| uparser-native | 1.000 | 27.117 | 33.965 | 35.8239 | 17854464 |

### office

| Engine | Success | Median ms | P95 ms | Docs/s | Peak RSS bytes |
|---|---:|---:|---:|---:|---:|
| anydoc | 1.000 | 13.996 | 16.662 | 70.3081 | 5775360 |
| uparser-native | 1.000 | 15.695 | 18.691 | 62.3963 | 6701056 |

## Gate Details

### pdf / pdf-inspector

- Performance failures: rss

| Quality metric | Mean paired delta | 95% bootstrap CI | Strictly better |
|---|---:|---:|---:|
| overall | 0.017568 | [0.010333, 0.026059] | true |
| nid | 0.004074 | [0.001741, 0.006975] | true |
| teds | 0.033789 | [0.008479, 0.068250] | true |
| mhs | 0.054717 | [0.029306, 0.084722] | true |

- Quality failures: none

### pdf / liteparse-text

- Performance failures: competitor_round_cv_at_most_3pct

| Quality metric | Mean paired delta | 95% bootstrap CI | Strictly better |
|---|---:|---:|---:|
| overall | 0.009460 | [-0.000486, 0.019836] | false |
| nid | 0.004760 | [-0.001961, 0.012627] | false |
| teds | 0.034732 | [0.000583, 0.070838] | true |
| mhs | 0.022841 | [-0.006377, 0.055377] | false |

- Quality failures: all_metric_ci_lower_bounds_above_zero, overall_lead_at_least_1pct

### office / anydoc

- Performance failures: median, median_10pct, p95, throughput, rss, elapsed_ratio_ci_upper_below_one, throughput_ratio_ci_lower_above_one
- Quality: paired semantic evaluations were not provided

## Interpretation

`FAIL` and `INSUFFICIENT` both block a comprehensive-leading claim. Quality gates use paired per-document deltas and bootstrap confidence intervals; missing semantic evaluations are never inferred from non-empty output.
