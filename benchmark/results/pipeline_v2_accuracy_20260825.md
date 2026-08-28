# Pipeline V2 精度评测（2026-08-25）

## 结论

Pipeline V2 已完成 OpenDataLoader-Bench 200 文档和 OmniDocBench v1.7 全量 1,651 页评测，均无推理失败；但两套基准的综合精度都低于官方 MinerU pipeline。当前实现证明了链路完整性，尚不能替代最新版 MinerU pipeline。

## OpenDataLoader-Bench

分数越高越好；官方行来自仓库保存的 MinerU 2.7.0 结果。

| 路径 | Overall | NID | NID-S | TEDS | TEDS-S | MHS | MHS-S |
|---|---:|---:|---:|---:|---:|---:|---:|
| Pipeline V2 | 0.800122 | 0.832794 | 0.814841 | 0.836368 | 0.916391 | 0.700005 | 0.874206 |
| 官方 MinerU 2.7.0 | 0.831135 | 0.857362 | 0.852723 | 0.872992 | 0.903697 | 0.742983 | 0.853625 |
| 差值 | -0.031014 | -0.024568 | -0.037881 | -0.036623 | +0.012694 | -0.042978 | +0.020582 |

Pipeline V2 为 200/200 成功，耗时 0.9706 秒/文档。官方速度为 Apple M4 上 5.9615 秒/文档，本次为 A100 服务，硬件和部署方式不同，不作速度优劣结论。

## OmniDocBench

使用 OmniDocBench v1.7 commit `193627ae9e97d89188468ed1ee3b7a856ff76044`。Edit 越低越好，CDM/TEDS/Overall 越高越好。

| 路径 | Overall | 文本 Edit | 公式 CDM | 表格 TEDS | TEDS-S | 阅读顺序 Edit |
|---|---:|---:|---:|---:|---:|---:|
| Pipeline V2 | 75.7789 | 0.190804 | 73.3099 | 73.1073 | 84.7475 | 0.294770 |
| 官方 MinerU-Pipeline | 86.47 | 0.055 | 83.07 | 81.88 | 88.68 | 0.153 |
| 差值 | -10.6911 | +0.135804 | -9.7601 | -8.7727 | -3.9325 | +0.141770 |

预测 1,651/1,651 成功，但有 60 页只包含空白 Markdown。官方 page match 有 2 页使用 timeout fallback；2,352 个 CDM 样本和 665 个 TEDS 样本均为 0 timeout、0 error、0 exception。

CDM 使用官方 v1.7 算法，但本机没有官方 ImageMagick/Ghostscript 环境，公式栅格化采用本地 TeX Live 2022 + `pdftocairo` 兼容路径，并出现 Euler 位图字体缺失警告。因此 CDM 可用于定位当前差距，不应宣称为官方 Docker 环境的严格数值复现。

## 模型差异

当前 Pipeline V2 使用 DocLayout-YOLO、YOLOv8-MFD、旧 PaddleOCR 权重、UniMERNet-small、仅无线表格 SLANet-plus 和 LayoutReader。最新版 MinerU pipeline 已采用 PPDocLayoutV2、PP-OCRv6、表格分类/方向及有线与无线模型组合；本次结果不是同权重框架对比。下一轮精度优化应优先升级模型资产并解决空白页与行内公式过度拆块。

机器可读结果见 `benchmark/results/pipeline_v2_accuracy_20260825.json`；原始评测位于 `benchmark/OmniDocBench/result/pipeline-v2-20260825_quick_match_*` 和 `opensource/opendataloader-bench/prediction/uparser-pipeline-v2-20260825/`。
