# uparser MinerU-VLM official accuracy comparison

Model: `MinerU2.5-Pro-2605-1.2B`; uparser: `0.4.0-rc.1`.

## Result

uparser MinerU-VLM is above the published MinerU 2.7 pipeline on every OpenDataLoader accuracy aggregate,
but below the official MinerU2.5-Pro result on OmniDocBench. Therefore V2 passes the existing regression
tolerance, but it is not an overall accuracy improvement over the official VLM baseline.

## OpenDataLoader Bench

All metrics are higher-is-better. Both runs cover the same 200 documents.

| Metric | uparser MinerU-VLM V2 | Official MinerU 2.7 | Delta |
|---|---:|---:|---:|
| Overall | 0.923978 | 0.831135 | +0.092842 |
| NID | 0.943310 | 0.857362 | +0.085948 |
| NID-S | 0.944279 | 0.852723 | +0.091557 |
| TEDS | 0.968228 | 0.872992 | +0.095237 |
| TEDS-S | 0.973958 | 0.903697 | +0.070261 |
| MHS | 0.867214 | 0.742983 | +0.124231 |
| MHS-S | 0.923655 | 0.853625 | +0.070031 |

The local run used uparser with a `MinerU2.5-Pro-2605-1.2B` A100 service. The published result is MinerU
2.7 pipeline on Apple M4. These are valid benchmark-output accuracy comparisons, but they do not isolate
uparser framework uplift and their wall-clock speeds must not be compared directly.

## OmniDocBench

The current official v1.7 evaluator re-evaluated all 1,651 local predictions. Edit metrics are
lower-is-better; Overall, CDM and TEDS metrics are higher-is-better.

| Metric | uparser MinerU-VLM V2 | Official MinerU2.5-Pro | Delta |
|---|---:|---:|---:|
| Overall | 91.4251 | 95.75 | -4.3249 |
| Text Edit | 0.069960 | 0.036 | +0.033960 |
| Formula CDM | 88.8433 | 97.45 | -8.6067 |
| Table TEDS | 92.4279 | 93.42 | -0.9921 |
| Table TEDS-S | 95.2449 | 95.92 | -0.6751 |
| Reading-order Edit | 0.136131 | 0.120 | +0.016131 |

The largest deficit is formula CDM. Table structure is much closer to the official result, while text and
reading order also regress. The leaderboard-compatible TEDS value above is page-averaged over 458 pages;
the evaluator's separate 665-table sample aggregate remains `0.906068`.

Evaluation health: page match used one timeout fallback; 2,352 formula CDM samples and 665 table TEDS
samples completed with zero timeout, error, or exception cases. CDM used the official v1.7 algorithm with
a local TeX Live 2022 plus `pdftocairo` compatibility path. Missing Euler bitmap-font warnings mean this
is not a byte-for-byte reproduction of the official Docker environment. The public leaderboard labels its
dataset `v1.6_full`, while the local 1,651-page data was evaluated with v1.7 code at the recorded commit;
the comparison is therefore a published-baseline comparison, not a claim of an identical packaged runtime.

## Historical V2 conclusion

Against the previous local `uparser-mineru-vlm` OpenDataLoader run, V2 changes Overall by `-0.004390`,
NID by `-0.003700`, TEDS by `+0.024334`, and MHS by `-0.010514`. It improves table quality but slightly
regresses aggregate, reading-order, and heading quality. Combined with the official OmniDocBench gap,
the release conclusion remains: architecture completeness and stability improved, overall VLM accuracy did not.
