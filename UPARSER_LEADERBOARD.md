# uparser 解析能力评测榜单

> 数据截止：2026-09-14 · 本文档只呈现**评测口径与对比数据**，不含归因分析与优化过程。
> 调试记录、缺陷归因与优化依据见 `BENCHMARK_REPORT.md` 与 `BENCHMARK_DEV_LOG.md`。

uparser 同时在两个**互相独立**的公开榜单上评测。两者语料、评测器与指标口径均不同，
**数字不可跨榜比较**，即使指标名称相同（如两边都有 "Table TEDS"）。

---

## 一、榜单说明

### 1.1 opendataloader-bench

| 项 | 说明 |
|---|---|
| 语料 | 200 篇单页真实 PDF |
| 位置 | `opensource/opendataloader-bench/pdfs` |
| 评测器 | 该项目自带 harness / `src/evaluator.py` |
| 输入形态 | PDF 原文件（解析器自行决定是否光栅化） |

**指标依据**

| 指标 | 含义 | 方向 |
|---|---|---|
| Reading Order (NID) | 归一化插入距离，衡量输出文本序列与真值的整体一致性（含顺序） | 越高越好 |
| Table (TEDS) | Tree-Edit-Distance-based Similarity，衡量表格结构与内容的还原度 | 越高越好 |
| Heading (MHS) | 标题层级匹配得分，衡量标题识别与层级还原 | 越高越好 |
| **Overall** | 上述三项**等权算术平均** | 越高越好 |
| Speed | 墙钟 ÷ 篇数（并发下的吞吐，非单篇延迟） | 越低越好 |

### 1.2 OmniDocBench

| 项 | 说明 |
|---|---|
| 语料 | 1651 页（v1.6 全量） |
| 位置 | `benchmark/OmniDocBenchData` |
| 评测器 | 官方 `run_eval.py`（`quick_match` 匹配策略） |
| 输入形态 | **页面图像**（无文本层） |

**指标依据**

| 指标 | 含义 | 方向 |
|---|---|---|
| Text Edit | 正文文本块的归一化编辑距离。仅对真值类别 `title` / `text_block` 计分，`header`/`footer`/`page_number` 为独立且被排除的类别 | 越低越好 |
| Formula Edit | 独立公式（`equation_isolated`）的编辑距离 | 越低越好 |
| Formula CDM | Character Detection Matching，将两侧 LaTeX 渲染为图像后比对，**需要可用的 TeX 环境**，否则对每个样本恒返回 0 | 越高越好 |
| Table TEDS | 表格结构+内容相似度 | 越高越好 |
| Table TEDS-S | 仅结构（忽略单元格文本） | 越高越好 |
| Reading Order Edit | 阅读顺序的编辑距离 | 越低越好 |

> **注**：OmniDocBench 的输入是页面图像，`native`（零模型、纯文本层）在该榜单上不适用，
> 不参与评测。

---

## 二、uparser 各执行模式与适配器

| 选择 | 类别 | 推理位置 | 依赖 |
|---|---|---|---|
| `native` | 零模型 | 进程内（纯 Rust） | 无外部服务 |
| `tesseract` | 本地 OCR | 进程内 | Tesseract 可执行文件与语言包 |
| `pipeline` | 多模型流水线 | 外置分阶段服务 | layout / OCR / formula / table 各阶段端点 |
| `mineru-vlm` | VLM 协议 | 外置 OpenAI 兼容端点 | MinerU2.5 系列 |
| `navidc-ocr` | VLM 协议 | 外置 OpenAI 兼容端点 | NaviDC-OCR |
| `monkeyocr-v2` | VLM 协议 | 外置 OpenAI 兼容端点 | MonkeyOCRv2-B-Parsing |
| `dots-ocr` | VLM 协议 | 外置 OpenAI 兼容端点 | dots.ocr |
| `paddleocr` | OCR 服务 | 外置 REST | PaddleOCR 服务 |
| `paddlex-structure` | 结构化服务 | 外置 REST | PP-StructureV3 服务 |
| `generic-vlm` | 通用 VLM | 外置 OpenAI 兼容端点 | 任意遵循 Markdown 输出约定的模型 |
| `auto` | 路由 | —— | 由内容预分析自动选择上列之一 |

---

<!--DATA_SECTION-->
