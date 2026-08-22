# UParser Competitor Benchmark Report

> Generated: `2026-08-22T11:27:33.777687+00:00`  
> Overall gate: **FAIL**

## Gate Summary

| Suite | Competitor | Performance/reliability | Quality | Overall |
|---|---|---:|---:|---:|
| pdf | pdf-inspector | FAIL | PASS | FAIL |
| pdf | liteparse-text | FAIL | FAIL | FAIL |

## Methodology

- CLI end-to-end timing includes process startup and output emission.
- Output channels match the paired competitor: stdout for PDF comparisons and in-process file output for the Office comparison.
- Runs use randomized Latin rotation; no startup subtraction or best-of-N selection.

## Measurements

### pdf

| Engine | Success | Median ms | P95 ms | Docs/s | Peak RSS bytes |
|---|---:|---:|---:|---:|---:|
| pdf-inspector | 0.995 | 47.726 | 92.064 | 18.3518 | 16146432 |
| liteparse-text | 1.000 | 60.809 | 78.190 | 16.0593 | 29061120 |
| uparser-native | 1.000 | 40.242 | 50.726 | 24.6469 | 18497536 |

## Gate Details

### pdf / pdf-inspector

- Performance failures: rss, candidate_round_cv_at_most_3pct, competitor_round_cv_at_most_3pct

| Quality metric | Mean paired delta | 95% bootstrap CI | Strictly better |
|---|---:|---:|---:|
| overall | 0.016400 | [0.009315, 0.024816] | true |
| nid | 0.003547 | [0.001322, 0.006449] | true |
| teds | 0.023954 | [0.000772, 0.057695] | true |
| mhs | 0.054566 | [0.029160, 0.084652] | true |

- Quality failures: none

### pdf / liteparse-text

- Performance failures: candidate_round_cv_at_most_3pct, competitor_round_cv_at_most_3pct

| Quality metric | Mean paired delta | 95% bootstrap CI | Strictly better |
|---|---:|---:|---:|
| overall | 0.008292 | [-0.001785, 0.018706] | false |
| nid | 0.004234 | [-0.002578, 0.012115] | false |
| teds | 0.024897 | [-0.012931, 0.063706] | false |
| mhs | 0.022689 | [-0.006569, 0.055302] | false |

- Quality failures: all_metric_ci_lower_bounds_above_zero, overall_lead_at_least_1pct

## Interpretation

`FAIL` and `INSUFFICIENT` both block a comprehensive-leading claim. Quality gates use paired per-document deltas and bootstrap confidence intervals; missing semantic evaluations are never inferred from non-empty output.
