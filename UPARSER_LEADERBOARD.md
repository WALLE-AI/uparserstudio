# uparser 解析能力评测榜单

> 数据截止：2026-09-21（monkeyocr-v2 两行经适配器对齐 + 文档组装对齐后全量重测，其余行沿用 2026-09-14）· 本文档只呈现**评测口径与对比数据**，不含归因分析与优化过程。
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

## 三、opendataloader-bench 对比（200 篇单页 PDF）

指标：Overall = NID / TEDS / MHS 等权均值。Speed 为并发吞吐（墙钟 ÷ 篇数）。

### 3.1 uparser 各模式

| 模式 | Overall ↑ | Reading Order (NID) ↑ | Table (TEDS) ↑ | Heading (MHS) ↑ | Speed s/篇 ↓ |
|---|---:|---:|---:|---:|---:|
| `mineru-vlm` | **0.9252** | 0.9415 | **0.9650** | **0.8771** | 0.682 |
| `pipeline` | 0.9086 | **0.9376** | 0.9133 | 0.8361 | 1.177 |
| `navidc-ocr` | 0.9053 | 0.9302 | 0.9645 | 0.8323 | 2.559 |
| `auto` | 0.8920 | 0.9280 | 0.9061 | 0.7948 | 0.137 |
| `monkeyocr-v2` | 0.8824 | 0.8942 | 0.9532 | 0.8296 | 2.497 |
| `native` | 0.8766 | 0.9197 | 0.8393 | 0.7826 | **0.044** |
| `dots-ocr` | 未评测 | — | — | — | — |
| `paddleocr` / `paddlex-structure` | 未评测 | — | — | — | — |
| `tesseract` | 未评测 | — | — | — | — |

### 3.2 外部参照

| 引擎 | Overall ↑ | NID ↑ | TEDS ↑ | MHS ↑ | Speed s/篇 ↓ |
|---|---:|---:|---:|---:|---:|
| opendataloader-hybrid | 0.9066 | 0.9337 | 0.9276 | 0.8208 | 0.463 |
| docling | 0.8817 | 0.8984 | 0.8871 | 0.8240 | 0.762 |
| pdf-inspector | 0.8754 | 0.9150 | 0.8141 | 0.7875 | 0.032 |
| marker | 0.8671 | 0.8898 | 0.8076 | 0.8190 | 53.932 |
| mineru | 0.8353 | 0.8574 | 0.8730 | 0.7588 | 5.962 |

---

## 四、OmniDocBench 对比（1651 页图像，v1.6 全量）

指标方向：Edit 类越低越好，CDM / TEDS 越高越好。

### 4.1 uparser 各模式

| 模式 | Text Edit ↓ | Formula Edit ↓ | Formula CDM ↑ | Table TEDS ↑ | TEDS-S ↑ | Reading Order Edit ↓ |
|---|---:|---:|---:|---:|---:|---:|
| `monkeyocr-v2` | **0.0499** | 0.1589 | 0.9398 | 0.8470 | 0.8851 | **0.1331** |
| `navidc-ocr` | 0.0593 | **0.0636** | **0.9631** | **0.9578** | **0.9641** | 0.1356 |
| `pipeline` | 0.0706 | 0.1652 | 0.9183 | 0.7868 | 0.8610 | 0.1530 |
| `mineru-vlm` | 0.0837 | 0.1042 | 0.9477 | 0.8797 | 0.9192 | 0.1448 |
| `dots-ocr` | 未评测 | — | — | — | — | — |
| `paddleocr` / `paddlex-structure` | 未评测 | — | — | — | — | — |
| `native` | 不适用 | — | — | — | — | — |

> `native` 为零模型、纯文本层引擎，本榜单输入为页面图像（无文本层），故不适用。

### 4.2 外部参照

| 引擎 | Text Edit ↓ | Formula CDM ↑ | Table TEDS ↑ | TEDS-S ↑ | Reading Order Edit ↓ |
|---|---:|---:|---:|---:|---:|
| NaviDC-OCR（官方发布值） | 0.027 | 0.9636 | 0.9705 | 0.9852 | 0.122 |
| MinerU2.5-Pro（官方发布值） | 0.036 | 0.9745 | 0.9342 | 0.9592 | 0.120 |
| mineru-vlm-2605（官方权重，本地同 harness 复跑） | 0.0377 | 未测 | 0.9200 | 0.9486 | 0.1296 |
| MinerU-Pipeline（官方发布值） | 0.055 | 0.8307 | 0.8188 | 0.8868 | 0.153 |

> 官方发布值取自各模型卡 / 论文，其运行环境与本地 harness 配置未必完全一致，
> 与上表 uparser 行并列仅作量级参照。

> **MonkeyOCRv2 官方并未公布 OmniDocBench 端到端解析成绩**（2026-09-21 核对其 README）。
> 它公布的两个相关数字都不能并入上表：
> ① 文档解析榜是 **MDPBench**（多语言，综合分 83.3，该榜第一），语料与指标都与本表无关；
> ② README 里唯一的 OmniDocBench 1.6 数字属于**独立的公式识别子模型**
>    （UniMERNet-T + MonkeyOCRv2-S backbone，在**真值公式裁剪图**上 CDM 90.8 / ExpRate 61.1），
>    与本表"整页端到端解析后再抽公式"的口径不同，不可直接比较。
>
> 本地 `monkeyocr-v2` 行跑的就是官方权重（`127.0.0.1:8011`，`MonkeyOCRv2-B-Parsing`），
> 所以该行的高低反映的是**适配器实现**，而非模型能力 —— 这也是 2026-09-20 那轮
> 文档组装对齐能把 Text Edit 从 0.0834 拉到 0.0499 的原因。

---

## 五、覆盖状态

| 模式 | opendataloader-bench | OmniDocBench | 阻塞项 |
|---|---|---|---|
| `native` | ✅ | 不适用 | — |
| `mineru-vlm` | ✅ | ✅ | — |
| `navidc-ocr` | ✅ | ✅ | — |
| `pipeline` | ✅ | ✅ | — |
| `auto` | ✅ | 未评测 | — |
| `monkeyocr-v2` | ✅ | ✅ | — |
| `dots-ocr` | ❌ | ❌ | 无可用端点 |
| `paddleocr` | ❌ | ❌ | 无可用服务 |
| `paddlex-structure` | ❌ | ❌ | 无可用服务 |
| `tesseract` | ❌ | ❌ | 未评测 |
| `generic-vlm` | ❌ | ❌ | 依赖具体模型，不设固定基线 |

---

## 六、复现

```bash
# opendataloader-bench
cd opensource/opendataloader-bench
uv run src/pdf_parser.py --engine <engine>
uv run src/evaluator.py

# OmniDocBench（CDM 需要可用 TeX，否则该项恒为 0）
cd benchmark
ls OmniDocBenchData/images/* | xargs -P 8 -I{} ./gen_<engine>.sh {}
cd OmniDocBench
TL=/path/to/texlive
PATH="$TL/bin/x86_64-linux:$PATH" CDM_TEXLIVE_ROOT="$TL" \
  CDM_PDFLATEX="$TL/bin/x86_64-linux/pdflatex" \
  .venv/bin/python run_eval.py --config configs/<config>.yaml
```

所有 uparser 行均由 **release 构建**的 CLI 子进程产出（`--no-cache --no-assets`）。

> 两点评测环境注记（均与被测引擎无关，但会让数字失真）：
> ① `ls OmniDocBenchData/images/*` 不要写成 `*.png` —— 1651 页里 981 页是 `.jpg`；
> ② `run_eval.py` 的公式候选匹配是无界递归，公式密集页面会以 `RecursionError`
> 终止整次评测。遇到时改用同参数的 `run_eval_deep.py`：它只抬高
> `sys.setrecursionlimit` 与 `threading.stack_size`，不改匹配／打分／配置，
> 未触发上限的样本两个入口结果逐字节一致。`monkeyocr-v2` 行即用后者产出。

