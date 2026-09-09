# uparser 解析引擎基准评测报告

> 最后更新：2026-09-09；当前 Pipeline V2 默认 profile：MinerU 3.4.5 + PP-DocLayoutV2 +
> PP-OCRv6 + PP-FormulaNet-plus-M。
> 该默认指整页 `documents/pages:analyze` 后端；`pipeline_v2.rs` 使用的 standalone
> stage endpoints 当前仍是 legacy-compatible 模型链，两者必须分开报分。
>
> **2026-09-09 双榜单统一复测**（Part A §7、Part B §3）：native / mineru-vlm / pipeline
> 三种模式在两个榜单上用同一脚本 `benchmark/run_dual_benchmark.py` 全部经 CLI 重跑。
> 该轮同时定位并修复了一个**公式渲染缺陷**，OmniDocBench Formula CDM 由 `40.1003`
> 恢复到 `92.1094`（mineru-vlm）、由 `79.2622` 恢复到 `91.1163`（pipeline）。
> 该缺陷随渲染器统一（M2/M3，把模型协议接入 canonical 渲染器）引入——在此之前
> `render.rs` 逐字输出 `Block.latex`，不经过转义器（见 `formula_repair::wrap_display_math`
> 的 D.10 注释），所以 2026-08 及更早的历史行不受影响；受影响的是渲染器统一之后、
> 本次修复之前产出的任何 Markdown。

本报告包含**两个互相独立的精度评测榜单**，以及**一个无真值的大文档性能测试**。三者的语料、评测器和指标口径不同，阅读时请先确认所在分区：

| | Part A:opendataloader-bench(§1–§7) | Part B:OmniDocBench(§1–§3) | Part C:大文档 OCR 性能 |
|---|---|---|---|
| 语料位置 | `opensource/opendataloader-bench/pdfs` | `benchmark/OmniDocBenchData` | `datasets/pdf/all_pdf/PDFs/data/黄陂…上册.pdf` |
| 语料规模 | 200 篇单页真实 PDF | 1651 页(全量)/ 290 页分层子集(prompt 实验) | 1 份、544 页、45,119,340 bytes |
| 官方评测器 | opendataloader-bench 自带 harness/evaluator | OmniDocBench 官方 `run_eval.py`(`quick_match`) | 无；`/usr/bin/time` 端到端计时 |
| 指标定义 | Reading Order=NID、Table=TEDS、Heading=MHS,Overall=三者等权均值 | Text/Reading Order=Edit_dist(越低越好)，Formula=CDM、Table=TEDS(越高越好)，Overall 按官方三项公式计算 | 墙钟、吞吐、峰值 RSS、输出覆盖；**不评精度** |
| 评测对象 | uparser V2 各模式、原始 MinerU 同模 Pipeline、MinerU 3.4.5 新模型 Pipeline 与公开榜单 | 当前 uparser V2、原始 MinerU 同模 Pipeline、MinerU 3.4.5、历史结果与官方参考值 | Rust Pipeline V2 分阶段路径 vs MinerU-VLM 2605 |
| 结论一句话(最新，2026-09-09 §7/§3) | mineru-vlm 0.9252 > pipeline 0.9086 > native 0.8766；pipeline 的 Rust CLI 分阶段路径已从 2026-08-26 的 0.74323 恢复并超过服务内 finalize 的 0.85789 | mineru-vlm 91.3490 / pipeline 88.3451；修复公式渲染缺陷后 Formula CDM 分别为 92.1094 / 91.1163，均高于此前历史最好值 | 两路径均 544/544 成功；当前配置下 VLM 墙钟快 4.314×，但并发不对称且服务端 `>=1000` 门槛未通过 |

Part A/B 之间的数字**不可跨表比较**(不同语料、不同评测器、不同指标口径),即使指标名字看起来一样(如都有"Table TEDS")。Part C 只是性能/稳定性测试，不得用其输出大小或结构数量代替精度指标。调试过程、探索性发现、失败尝试的完整记录见 `BENCHMARK_DEV_LOG.md`——本报告只保留干净的榜单结果与结论。

---

# Part A:opendataloader-bench 榜单

> 原始榜单日期:2026-08-04；V2 复测日期:2026-08-21；Pipeline 最终复测:2026-08-26 · 语料:opendataloader-bench,200 篇单页真实 PDF · 评测器:同一 harness/evaluator
> 指标:Reading Order = NID、Table = TEDS、Heading = MHS,**Overall = 三者等权均值**;Speed = s/篇(越低越好)

---

## 1. 结论速览

| 引擎 | Overall | Reading Order | Table | Heading | 速度 s/篇 | 依赖 |
|---|---|---|---|---|---|---|
| uparser V2 · mineru-vlm 2605(无缓存) | **0.9240** | **0.9433** | **0.9682** | **0.8672** | **0.621**(GPU) | vLLM(MinerU2.5-Pro-2605) |
| uparser · mineru-vlm(历史冻结) | **0.9284** | **0.9470** | **0.9439** | **0.8777** | 1.81(GPU) | 更早 MinerU2.5 配置 |
| opendataloader-hybrid(榜首参照) | 0.907 | 0.934 | 0.928 | 0.821 | 0.463 | 布局模型 |
| uparser V2 · auto | 0.8920 | 0.9280 | 0.9061 | 0.7948 | 0.137 | native 156 / VLM 44 |
| docling(参照) | 0.882 | 0.898 | 0.887 | 0.824 | 0.762 | 模型 |
| **uparser V2 · native** | 0.8754 | 0.9150 | 0.8141 | 0.7875 | **0.051** | **零模型/纯 Rust** |
| pdf-inspector(baseline) | 0.8754 | 0.9150 | 0.8141 | 0.7875 | 0.032 | 纯 Rust |
| edgeparse(参照) | 0.837 | 0.894 | 0.717 | 0.706 | 0.036 | 纯 Rust |
| **官方 MinerU 2.7** | 0.8311 | 0.8574 | 0.8730 | 0.7430 | 5.962(Apple M4) | 官方 pipeline |
| **原始 MinerU 1.3.5 Pipeline(严格同模)** | **0.8530** | **0.8806** | **0.8564** | **0.7862** | 1.227(A100) | 与 uparser 相同旧权重 |
| **MinerU 3.4.5 Pipeline** | **0.8568** | **0.8745** | **0.9107** | **0.7918** | 0.716(A100) | PP-DocLayoutV2 新模型链 |
| **uparser Pipeline V2 · MinerU 3.4.5 + PP-Formula** | **0.8579** | **0.8755** | **0.9129** | **0.7924** | 1.286(A100) | 文档 API + 官方 finalize |
| **uparser Pipeline V2(旧权重)** | 0.8001 | 0.8328 | 0.8364 | 0.7000 | 0.971(A100) | 六阶段 pipeline |
| **uparser Pipeline V2 · Rust CLI staged(2026-08-26)** | **0.7432** | **0.8053** | **0.2768** | **0.7201** | 0.986(A100) | `pipeline_v2.rs` + legacy profile 阶段服务 |
| **liteparse(榜单)** | 0.576 | 0.866 | 0.000 | 0.000 | 1.061 | PDFium+OCR |
| **↓ 2026-09-09 统一复测(§7)，与上表不同批次，勿混排** | | | | | | |
| **uparser · mineru-vlm(2026-09-09)** | **0.9252** | **0.9415** | **0.9650** | **0.8771** | 0.682(4 workers) | vLLM(MinerU2.5-Pro-2605) |
| **uparser · pipeline(2026-09-09)** | **0.9086** | **0.9376** | **0.9133** | **0.8361** | 1.177(8 workers) | `pipeline_v2.rs` + bare tensor 服务 |
| **uparser · native(2026-09-09)** | **0.8766** | **0.9197** | **0.8393** | **0.7826** | **0.044**(1 worker) | **零模型/纯 Rust** |

**主结论:**
1. **当前 V2 mineru-vlm 仍超过外部榜首 hybrid**(Overall 0.924 vs 0.907)，并以 Table TEDS `0.9682` 超过历史冻结结果 `0.9439`；但 Overall、Reading Order 和 Heading 分别回退 `0.0044`、`0.0037`、`0.0105`。回退均小于 V2 发布闸门 `0.02`，但不能把历史 `0.9284` 写成当前 V2 分数。
2. **V2 native 与冻结输出和质量逐文件一致**，统一 runner 的无缓存开销从 `0.0473` 增至 `0.0508 s/篇`，约 `+7.33%`；仍在精度与速度两个维度超过 liteparse。
3. **V2 auto 是折中档**：156 篇走 native、44 篇走 mineru-vlm，Overall `0.8920`，比纯 native 高 `0.0166`，速度约为全量 VLM 的 `4.53x`。
4. 历史 mineru-vlm 分数含一次渲染层 bug 修复(Overall 0.708→0.928);发现与修复过程见 `BENCHMARK_DEV_LOG.md` §1。当前 V2 使用不同 checkpoint，历史与当前比较是发布回归门槛，不是纯框架微基准。
5. **严格同模 A/B 证明 Pipeline V2 有重构损失**：同一 200 PDF、同一旧权重下，相对原始 MinerU
   Pipeline，Overall/NID/TEDS/MHS 分别回退 `0.052859`/`0.047814`/`0.020020`/`0.086222`。
   因模型输入保持不变，这部分不能归因于 PP-DocLayoutV2 等新旧模型差异。
6. **新 profile 消除了旧 V2 的装配损失**：ODL Overall `0.857889`，较本地原始 MinerU 3.4.5
   `0.856821` 高 `0.001067`。由于 PaddleOCR 存在小幅非确定性，这证明服务/API 路径未造成可见回退，
   但不能把千分之一差距解释为统计显著的算法提升。
7. **Rust 分阶段路径仍有明显装配损失**（2026-08-26 批次；2026-09-09 复测已不复现，见 §7）：该批次用 CLI 直接经过
   `pipeline_v2.rs` 处理 200/200 PDF，Overall 为 `0.743227`，较旧 V2 HTTP 路径低
   `0.056895`，较原始同模 MinerU 低 `0.109754`。最大问题是 TEDS 从旧 V2 的
   `0.836368` 降至 `0.276770`，而 TEDS-S 仍为 `0.918768`，表明表格结构大致存在，
   单元格内容绑定/渲染严重错位。

---

## 2. 分项分析

### 2.1 Reading Order(NID)
- 当前 V2 mineru-vlm **0.943** > V2 auto 0.928 > native/pdf-inspector 0.915 > liteparse 0.866；历史冻结 mineru-vlm 为 0.947。
- VLM 的版面理解让阅读顺序最优;native 的纯几何 XY-cut/投影已很强(0.915),远好于 liteparse。

### 2.2 Table(TEDS)
- 当前 V2 mineru-vlm **0.968**(OTSL→HTML) > 历史冻结 0.944 > 外部 hybrid 0.928；native 为 0.814(pdf-inspector 三策略表格检测)。
- 差距 0.13 是 native 相对天花板的主要短板;但 native 的 0.814 已优于 edgeparse(0.717)、unstructured 等,且**远优于 liteparse 的 0.000**。

### 2.3 Heading(MHS)
- 当前 V2 mineru-vlm **0.867** > docling 0.824 > native 0.788；历史冻结 mineru-vlm 为 0.878。
- MHS 指标**层级无关**(源码:"treats all heading levels as equivalent",APTED 只比 heading/content 标签,不比 `#` 层数)——所以关键是"该是标题的行有没有被标成标题",而非层级深浅。native 的短板是标题**过检测**(引擎产 280 个 vs GT 193 个)。

### 2.4 速度
- 当前 V2 native 无缓存运行 **0.0508 s/篇**(纯 Rust、零模型、无 GPU)，与最快的纯 Rust 引擎同档。
- 当前 V2 mineru-vlm 无缓存运行 **0.6208 s/篇**；V2 auto 为 **0.1370 s/篇**。历史 `1.81 s/篇` 和缓存命中 `0.005 s/篇` 不再作为当前性能结论。
- 新 Pipeline V2 为 **1.2862 s/篇**(A100、2 workers、文档 API)，旧权重 V2 为 `0.9706 s/篇`；
  官方 MinerU 2.7 为 **5.9615 s/篇**(Apple M4)。硬件和模型链不同，不能用墙钟差值宣称加速。

### 2.5 MinerU-VLM / Pipeline 完整对比

全部指标越高越好；各本地路径均覆盖 200/200 文档且无预测失败。

| 路径 | Overall | NID | NID-S | TEDS | TEDS-S | MHS | MHS-S |
|---|---:|---:|---:|---:|---:|---:|---:|
| **uparser MinerU-VLM V2** | **0.923978** | **0.943310** | **0.944279** | **0.968228** | **0.973958** | **0.867214** | **0.923655** |
| 官方 MinerU 2.7 | 0.831135 | 0.857362 | 0.852723 | 0.872992 | 0.903697 | 0.742983 | 0.853625 |
| MinerU-VLM 差值 | +0.092842 | +0.085948 | +0.091557 | +0.095237 | +0.070261 | +0.124231 | +0.070031 |
| **uparser Pipeline V2** | **0.800122** | **0.832794** | **0.814841** | **0.836368** | **0.916391** | **0.700005** | **0.874206** |
| Pipeline 差值 vs 官方 | -0.031014 | -0.024568 | -0.037881 | -0.036623 | +0.012694 | -0.042978 | +0.020582 |
| **uparser Pipeline V2 · Rust CLI staged(2026-08-26)** | **0.743227** | **0.805322** | **0.819593** | **0.276770** | **0.918768** | **0.720052** | **0.877049** |
| Rust staged - 旧 V2 | **-0.056895** | -0.027472 | +0.004752 | **-0.559598** | +0.002377 | +0.020047 | +0.002843 |
| **原始 MinerU 1.3.5 Pipeline(同模)** | **0.852981** | **0.880607** | **0.858084** | **0.856388** | **0.916453** | **0.786227** | **0.872702** |
| uparser - 原始同模 | **-0.052859** | **-0.047814** | **-0.043242** | **-0.020020** | **-0.000062** | **-0.086222** | **+0.001505** |
| **MinerU 3.4.5 Pipeline** | **0.856821** | **0.874540** | **0.868748** | **0.910738** | **0.936758** | **0.791773** | **0.864898** |
| 3.4.5 - 原始同模 | +0.003841 | -0.006067 | +0.010664 | +0.054351 | +0.020305 | +0.005546 | -0.007804 |
| **uparser Pipeline V2 · 3.4.5 + PP-Formula** | **0.857889** | **0.875516** | **0.869862** | **0.912943** | **0.938531** | **0.792361** | **0.864898** |
| uparser latest - MinerU 3.4.5 | **+0.001067** | +0.000977 | +0.001114 | +0.002205 | +0.001773 | +0.000588 | +0.000000 |

严格消融使用 `magic-pdf 1.3.5` 作为原始实现，因为它能原样加载 `magic-pdf.json` 中与 uparser
Pipeline V2 完全相同的 DocLayout-YOLO、YOLO MFD、UniMERNet-small、PaddleOCR、SLANet-plus 和
LayoutReader 权重。MinerU 3.4.5 强制使用 PP-DocLayoutV2 等新资产，因此单列为“新模型 + 新
Pipeline”系统比较，不能用它归因 uparser 重构本身。

MinerU 3.4.5 相对公开 MinerU 2.7 ODL 参考的 Overall/NID/TEDS/MHS 分别为 `+0.025686`/
`+0.017178`/`+0.037747`/`+0.048790`；两者版本与硬件不同，该差值只表示同一评测器下的产物精度。

### 2.6 Pipeline V2 精度偏低的原因

本次同模 A/B 排除了“只是模型旧”的解释，剩余差距集中在模型输出之后：

- uparser benchmark renderer 直接按区域把 OCR span 用换行连接，仅处理两级标题；原始 MinerU 还会做
  低置信度/高 IoU 检测去重、重叠 span 清理、block 修复、标题合并、LayoutReader 行排序和段落切分。
- 原始 Markdown builder 具有语言感知空格、英文行末断词合并、Markdown 转义和标题层级恢复；uparser
  当前没有等价的 middle-json/paragraph reconstruction 层。这与 NID、MHS 的最大回退一致。
- uparser 表格链按 bbox 中心选择整页 OCR span，没有稳定的唯一父区域/消费关系。重叠 table region
  可重复使用同一批 span；实测 `page-07d333cf-4ebf-427c-b146-e419962b81fa.md` 中同一表格被多次输出，
  uparser 为 9,630 字符，原始 MinerU 为 1,664 字符。
- 全量 Omni 输出中，uparser 总字符数比原始同模多 `15.43%`，空白页 `60` 对 `2`，重复非空行
  excess 占比约 `12.44%` 对 `7.65%`。这些产物级异常与上述实现缺口一致。

---

## 3. native vs liteparse:目标达成情况

用户目标之一是"native 从精度与速度全面超过 liteparse":

| | native | liteparse(榜单) | 结果 |
|---|---|---|---|
| Overall | **0.8754** | 0.576 | 超 +0.30 |
| Reading Order | **0.915** | 0.866 | 超 |
| Table | **0.814** | 0.000 | 超 |
| Heading | **0.788** | 0.000 | 超 |
| 速度 s/篇 | **0.0508** | 1.061 | 快 ~21× |

**精度与速度双维度全面超越,✅ 达成。**(注:榜单 liteparse 用的是纯文本口径;即便给它最乐观的 markdown 估分,native 仍稳赢,且速度优势是结构性的。)

## 4. native vs pdf-inspector:内部化 + 打平

native 的 Markdown 当前直通内嵌引擎(即 pdf-inspector 核心)，V2 与冻结输出**逐字节相同**，故 Overall **打平**(均 0.8754)。

> **2026-09-07 更新（M3 渲染器合并 2→1）**：native 的默认 Markdown 已改为
> `--markdown-source canonical`（统一渲染器，不再直通引擎字符串），同一评测器同一 200 篇下
> **Overall 0.8766 / NID 0.9197 / TEDS 0.8393 / MHS 0.7826**，相对 pdf-inspector 的 0.8754
> **+0.0012**——本节末尾「要严格超越 pdf-inspector 仍需引擎核心调优」的那条待办，是靠把引擎
> 已有的结构判断导出到 IR（表格流内位置、标题对账、链接注释剔除、行内清理）达成的，
> 没有改引擎核心。引擎自身的 Markdown 保留为 `--markdown-source engine-legacy`，
> 200 篇逐字节不变。详见 `RENDERER_UNIFICATION_EXECUTION_PLAN.md` §18。V2 统一 runner 无缓存速度为 `0.0508 s/篇`，冻结 native 为 `0.0473 s/篇`，独立 pdf-inspector baseline 为 `0.032 s/篇`。native 的价值在于去除 liteparse 依赖、消除 PDFium 二进制并纳入统一 IR/CLI/路由；要严格超越 pdf-inspector 仍需引擎核心调优。

---

## 5. 选型建议

| 场景 | 推荐 | 理由 |
|---|---|---|
| 电子版 PDF、无 GPU、要快 | **native** | 0.0508 s/篇、零依赖、0.875 分，明显超过 liteparse |
| 追求当前 V2 最高质量、有 GPU | **mineru-vlm** | Bench A Overall 0.924、Table TEDS 0.968；Omni Overall 91.4251，仍高于纯 Pipeline 88.4489 |
| 扫描件 | **mineru-vlm** | native 无 OCR(扫描件会空);VLM 直接识别 |
| `--protocol auto` | 由 profiler 路由 | 电子版长文本→native,表格密集/扫描→VLM |
| 六阶段可解释模型链 | **pipeline latest** | ODL 已对齐并数值超过本地 MinerU 3.4.5；Omni 结论见 Part B |

---

## 6. 方法学与可复现

- 语料:`opensource/opendataloader-bench/pdfs`(200 篇单页)。
- 接入:新增 `src/pdf_parser_uparser_native.py`、`pdf_parser_uparser_mineru_vlm.py`、`pdf_parser_pdf_inspector.py`,注册进 `engine_registry.py`。
- 运行:`python src/run.py --engine <name> --force` → `prediction/<name>/evaluation.json`。
- 二进制:`cargo build --release --features native,pdfium`(native 走 lopdf 引擎;mineru-vlm 走 pdfium 光栅化 + `http://127.0.0.1:19122` 的 MinerU2.5 vLLM endpoint)。
- pdf-inspector baseline:其自带 `cargo build --release --bin pdf2md`。
- Pipeline V2：`python3 benchmark/run_pipeline_v2_benchmarks.py`；结构化结果见
  `benchmark/results/pipeline_v2_accuracy_20260825.json`。
- 2026-08-26 Rust CLI staged：200 篇预测和官方评分分别见
  `opensource/opendataloader-bench/prediction/uparser-pipeline-v2-rust-cli-staged-20260826/summary.json`
  和同目录的 `evaluation.json`。
- 原始同模复跑：`benchmark/run_original_mineru_pipeline.py`；MinerU 3.4.5 复跑：
  `benchmark/run_mineru_345_pipeline.py`；三方结构化结果见
  `benchmark/results/pipeline_comparison_20260825.json`。

### 局限
- native 无 OCR:扫描件(如 doc `01030000000141`,image_dense)输出为空——这是设计取舍(扫描件交由 VLM 路由),pdf-inspector 同引擎同样为空。
- 当前 V2 mineru-vlm 分数依赖 `MinerU2.5-Pro-2605-1.2B`；历史冻结结果使用更早 checkpoint/configuration，不能当作纯框架 A/B。
- 榜单参照值(hybrid/docling/mineru/liteparse)取自 bench README；V2 native/mineru-vlm/auto 与历史冻结结果均为本地实测，但运行日期和模型配置不同。
- Pipeline V2 默认已使用 MinerU 3.4.5 + PP-DocLayoutV2 + PP-FormulaNet-plus-M；
  `magic-pdf.json` 旧权重只保留作严格同模消融。OpenDataLoader 官方速度来自 Apple M4，
  本地 VLM/Pipeline 来自 A100。
- `0.857889/88.4489` 是 V2 模型服务 `documents:analyze/pages:analyze` 复用 MinerU 官方 finalize
  的已验证结果。Rust `pipeline_v2.rs` 现已独立编排 layout→MFD→OCR→MFR→table→assemble→order，
  不加载模型包；本次已用 release CLI 单独复测，其 ODL Overall 为 `0.743227`，
  不能继承服务内 finalize 的 `0.857889`。
- 防过拟合:未针对本语料调参;渲染器修复是通用正确性修复(所有 VLM 协议受益),非针对 GT 的 tuning。

---

## 7. 2026-09-09 统一复测:native / mineru-vlm / pipeline

> 语料与评测器同 §1–§6（200 篇、`src/evaluator.py`）。三种模式**全部经 `uparser` CLI
> 子进程产出预测**（`--no-cache --no-assets`），由 `benchmark/run_dual_benchmark.py`
> 统一驱动；与 §1 上半张表是不同批次、不同二进制，不要混排成一张榜。

| 模式 | Overall | NID | TEDS | MHS | s/篇 | 生成 |
|---|---:|---:|---:|---:|---:|---|
| mineru-vlm | **0.9252** | 0.9415 | 0.9650 | 0.8771 | 0.682 | 200/200 |
| pipeline | 0.9086 | 0.9376 | 0.9133 | 0.8361 | 1.177 | 200/200 |
| native | 0.8766 | 0.9197 | 0.8393 | 0.7826 | **0.044** | 199/200 |

**结论：**

1. **pipeline 的 Rust CLI 分阶段路径不再有装配损失**。同为 `pipeline_v2.rs` 经 CLI 的路径，
   Overall 由 2026-08-26 的 `0.7432` 升至 `0.9086`，TEDS 由 `0.2768` 升至 `0.9133`
   ——§1 结论 7 记录的"表格内容装配严重错位"在当前 bare tensor 服务 + MinerU 3.4.5
   profile 下不再复现。注意两次的**服务端 profile 不同**（legacy vs mineru-3.4.5 bare
   tensor），因此这不是纯 Rust 侧的 A/B。
2. **pipeline 已超过服务内 finalize 路径**（`0.9086` vs `0.857889`），也超过本地原始
   MinerU 3.4.5 Pipeline（`0.856821`）。
3. **native 与 M3 渲染器统一后的冻结值逐位一致**（`0.876629/0.919656/0.839317/0.782614`，
   修复前后两轮也逐位一致）。native 同样走 canonical 渲染器，但它的块不带 `latex`，
   本语料上也没有触发行内公式改写——所以这是"实测零影响"，不是"结构上不可能受影响"。
4. **公式修复对本榜单几乎无影响**（mineru-vlm `0.9251→0.9252`、pipeline
   `0.9086→0.9086`）：ODL 的三项指标里没有公式项。这恰好是该缺陷能长期潜伏的原因——
   只看 Part A 是发现不了的。

速度栏是**并发下的吞吐**（墙钟 ÷ 篇数，worker 数见 §1 表），不是单篇延迟，不能与
单 worker 的历史行直接相减。

---

# Part B:OmniDocBench 榜单

> 与 Part A(opendataloader-bench)是**完全不同的语料和评测体系**,数字不可跨 Part 比较——见文首对照表。

## 1. OmniDocBench 全量(1651 页)榜单:当前 V2 vs 历史与外部参照

> 语料:`benchmark/OmniDocBenchData/OmniDocBench.json`,**全量 1651 页**(与 Part A 的 opendataloader-bench 200 篇是**不同语料**,不可跨表比较)
> 评测器:OmniDocBench 官方 `run_eval.py`(`quick_match`)
> 当前 MinerU-VLM/Pipeline 使用 OmniDocBench v1.7 evaluator、quick-match 24 workers、CDM 8 workers、TEDS 24 workers，可计算官方 Overall。Qwen3.8-27B 历史实验仍只有 Edit/TEDS 指标，不与 Overall/CDM 表混算。Qwen 经 `127.0.0.1:8094` 纯净端点生成；旧 `:8087` 端口混用问题见 `BENCHMARK_DEV_LOG.md` §2.4。

### 1.1 当前 V2 全量复测（2026-08-21 生成，2026-08-25 evaluator/CDM 复核）

当前 release 使用 `mineru-vlm` 协议、`MinerU2.5-Pro-2605-1.2B`、4 个生成 worker 和 `--no-cache`。
1,651/1,651 页均生成成功，非零返回码 0；其中 1 页 Markdown 仅含换行。2026-08-25 evaluator
复核中 page match 有 1 页使用 timeout fallback；2,352 个 CDM 和 665 个 TEDS 样本均为
0 timeout、0 error、0 exception。

| 指标(quick_match) | **当前 uparser V2** | 历史 mineru-vlm-2605-surpass-e1-full | **Qwen3.8-27B(纯净实测)** | Qwen3-VL-235B(官方参考) |
|---|---:|---:|---:|---:|
| Text Edit↓ | 0.0700 | **0.0367** | 0.0481 | 0.0630 |
| Formula Edit↓(非 CDM,不可直接对比官方列) | 0.1026 | **0.0948** | 0.1614 | — |
| Table TEDS↑ | **0.9061** | **0.9065** | 0.7920 | 0.8307 |
| Table TEDS-S↑ | **0.9375** | **0.9388** | 0.8259 | 0.8675 |
| Reading Order Edit↓ | 0.1361 | **0.1285** | 0.1522 | 0.1660 |

当前 V2 相对历史 surpass：Text Edit 退化 `+0.0332`、Formula Edit 退化 `+0.0077`、Table TEDS
下降 `0.0004`、TEDS-S 下降 `0.0014`、Reading Order Edit 退化 `+0.0077`。表格基本持平，但文本
和阅读顺序回退明确，因此当前 V2 的结论是**运行完整性与稳定性通过，质量未超过历史最佳基线**。

相对 Qwen3.8-27B，当前 V2 的 Formula Edit 改善 `0.0588`、Table TEDS 提升 `0.1141`、TEDS-S
提升 `0.1116`、Reading Order Edit 改善 `0.0165`，但 Text Edit 较差 `0.0216`。这说明文档专用模型
在公式、表格和顺序上优势明显，纯文本识别并非当前链路的强项。

### 1.2 MinerU-VLM 与 Pipeline 官方对比

Edit 越低越好，其他指标越高越好。下表使用 leaderboard 的 page-average TEDS 口径；因此
MinerU-VLM 的 `92.4279` 与上表 665 个表格 sample aggregate `0.9061` 数值尺度和聚合方式不同。

| 路径 | Overall↑ | Text Edit↓ | Formula CDM↑ | Table TEDS↑ | TEDS-S↑ | Order Edit↓ |
|---|---:|---:|---:|---:|---:|---:|
| **uparser MinerU-VLM V2** | **91.4251** | **0.069960** | **88.8433** | **92.4279** | **95.2449** | **0.136131** |
| 官方 MinerU2.5-Pro | 95.75 | 0.036 | 97.45 | 93.42 | 95.92 | 0.120 |
| MinerU-VLM 差值 | -4.3249 | +0.033960 | -8.6067 | -0.9921 | -0.6751 | +0.016131 |
| **uparser Pipeline V2(旧权重)** | **75.7789** | **0.190804** | **73.3099** | **73.1073** | **84.7475** | **0.294770** |
| **uparser Pipeline V2 · Rust CLI staged(2026-08-26)** | **58.6162** | **0.217967** | **70.1110** | **27.5343** | **84.7622** | **0.288339** |
| 官方 MinerU-Pipeline | 86.47 | 0.055 | 83.07 | 81.88 | 88.68 | 0.153 |
| 旧 V2 差值 vs 官方 | -10.6911 | +0.135804 | -9.7601 | -8.7727 | -3.9325 | +0.141770 |
| Rust staged 差值 vs 官方 | **-27.8538** | **+0.162967** | **-12.9590** | **-54.3457** | **-3.9178** | **+0.135339** |
| **本地 MinerU 3.4.5 Pipeline** | **85.3391** | **0.056176** | **79.5826** | **82.0523** | **88.8441** | **0.153534** |
| 3.4.5 差值 vs 官方 | -1.1309 | +0.001176 | -3.4874 | +0.1723 | +0.1641 | +0.000534 |
| 本地 MinerU 3.4.5 + PP-Formula（直接候选） | 88.5123 | 0.054230 | 88.5788 | 82.3811 | 88.8515 | 0.147369 |
| **uparser Pipeline V2 · 3.4.5 + PP-Formula** | **88.4489** | **0.054134** | **88.4867** | **82.2733** | **88.6950** | **0.147122** |
| uparser latest 差值 vs 官方 Pipeline | **+1.9789** | **-0.000866** | **+5.4167** | **+0.3933** | **+0.0150** | **-0.005878** |
| uparser latest 差值 vs 直接 PP 候选 | -0.0634 | -0.000096 | -0.0921 | -0.1078 | -0.1565 | -0.000248 |

MinerU-VLM 的最大差距是 Formula CDM `-8.6067`，表格 TEDS 仅低 `0.9921`；结论是完整性通过，
官方 VLM 精度对齐不通过。Pipeline V2 1,651/1,651 预测成功，但有 60 页仅为空白 Markdown；
page match 有 2 个 fallback，2,352 个 CDM 和 665 个 TEDS 样本均无 timeout/error/exception。
Pipeline 生成墙钟为 `3,866.42s`、`2.34247s/page`；六项指标均未达到官方 MinerU-Pipeline，
当前旧权重实现只能作为可运行基线。

本地 MinerU 3.4.5 使用 PP-DocLayoutV2、表格方向分类、有线 UnetStructure 和无线 SLANet-plus
完整新模型链；1,651/1,651 生成成功，仅 1 个空白页，生成 `1.00317s/page`。正式评测的 page
fallback、CDM/TEDS timeout/error/exception 均为 0。其 Overall 比 uparser Pipeline V2 高
`9.5602` 分，距官方参考仍有 `1.1309` 分；表格 TEDS/TEDS-S 已分别高 `0.1723`/`0.1641` 分，
主要剩余差距是公式 CDM `-3.4874` 分。

uparser Pipeline V2 的默认服务 profile 已切换为 MinerU 3.4.5 + PP-DocLayoutV2 + PP-OCRv6 +
PP-FormulaNet-plus-M，并新增 `documents:analyze` 原 PDF 接口、MinerU processing-window 原生 batch
和权威 Markdown 返回。最终服务路径 1,651/1,651 页生成成功，仅 1 页空白；累计生成
`2,128.36s`，即 `1.28914s/page`。首次生成中 128 页因服务的 5,000 万像素安全上限被拒绝，
在可信评测输入上以 `UPARSER_PIPELINE_MAX_IMAGE_PIXELS=200000000` 恢复后全部成功；生产默认上限
仍保持 5,000 万。最终 evaluator 的页面匹配、2,352 个 CDM 和 665 个 TEDS 样本均为 0 timeout、
0 error、0 exception、0 fallback。

最终 Overall `88.4489` 比官方 MinerU-Pipeline 高 `1.9789` 分，比旧 uparser Pipeline V2 高
`12.6700` 分，比本地 MinerU 3.4.5 UniMER 默认链高 `3.1098` 分。相对相同 PP-Formula 配置的
MinerU 直接候选仅低 `0.0634` 分，文本和阅读顺序 Edit 反而分别改善 `0.000096`/`0.000248`；
该微小波动不作统计显著提升解释。结论是 **Pipeline 类基线已超过**，但纯 Pipeline 仍低于
uparser MinerU-VLM `91.4251` 和官方 MinerU2.5-Pro `95.75`；若“所有基线”包含 VLM，必须增加
质量路由或 VLM fallback，不能声称固定 Pipeline 已达到该门槛。

2026-08-26 新增的 **Rust CLI staged** 行才是当时
`uparser/crates/uparser-core/src/adapters/pipeline_v2.rs` 的实际路径：release CLI 独立调度
layout→MFD→OCR→MFR→table→assemble→order，模型服务只提供阶段推理。因为当前
服务的 standalone stage endpoints 仍是 legacy-compatible 后端，本行使用
DocLayout-YOLO、YOLOv8-MFD、PaddleOCR、UniMERNet-small 和 SLANet-plus，**不是**
MinerU 3.4.5 + PP-DocLayoutV2 的内部整页 finalize 路径。

1,651/1,651 页生成成功、无 CLI 失败，但有 60 页仅含空白字符。首轮在 794 页后
因 5,000 万像素安全上限中断；对可信评测输入设置
`UPARSER_PIPELINE_MAX_IMAGE_PIXELS=200000000` 后全部补齐。`summary.json` 的 `2,149.52s`
只是最终 resume 段，不是完整生成墙钟，因此本次不报一个伪造的全程 s/page。
官方 evaluator 覆盖 1,651 页，page timeout fallback 1；2,352 个 CDM 和 665 个 TEDS
样本均为 0 timeout、0 error、0 exception。

Overall `58.6162` 较旧 uparser Pipeline V2 低 `17.1627` 分，较官方 MinerU-Pipeline
低 `27.8538` 分。其中 Table TEDS 仅 `27.5343`，但结构指标 TEDS-S 为
`84.7622`；这与 ODL 的 TEDS/TEDS-S `0.276770/0.918768` 形成同样的强烈分化，
可将问题收敛到当前 staged 表格链的 table region→全页 OCR span→table service→
HTML/Markdown 内容绑定，而不是单纯的表格结构检测失败。要继续区分
`pipeline_v2.rs` 的请求组装与 table endpoint 内部识别的责任，需对失败页保存阶段 trace
并对照原始 MinerU 的 cell-level 中间产物。

### 1.3 原始 MinerU 与 uparser Pipeline V2 严格同模 A/B

两条路径对同一 1,651 页使用相同旧权重、`ocr` 方法、同一 v1.7 evaluator；因此下表的差值可用于
判断 uparser 重构本身。Edit 越低越好，其他指标越高越好。

| 路径 | Overall↑ | Text Edit↓ | Formula CDM↑ | Table TEDS↑ | TEDS-S↑ | Order Edit↓ | 空白页 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 原始 MinerU 1.3.5 Pipeline | **76.1250** | **0.153716** | 68.6217 | **75.1248** | 82.5540 | **0.266186** | **2** |
| uparser Pipeline V2 | 75.7789 | 0.190804 | **73.3099** | 73.1073 | **84.7475** | 0.294770 | 60 |
| uparser - 原始 | **-0.3460** | **+0.037088** | **+4.6882** | **-2.0175** | **+2.1935** | **+0.028584** | **+58** |

这不是单向全面回退：uparser 的公式 CDM 和表格结构 TEDS-S 更高；但文本、阅读顺序、表格内容
TEDS 和空白页明显更差，最终 Overall 回退 `0.3460` 分。结合 ODL 的 `-0.052859`，可确认重构
损失主要在文本/标题/顺序和表格内容装配，而不是所有模型阶段都退化。

CDM 使用官方 v1.7 算法，但本机通过 TeX Live 2022 + `pdftocairo` 兼容路径栅格化，并出现 Euler
位图字体缺失警告。公开榜单标记为 `v1.6_full`，本地数据使用记录 commit 的 v1.7 evaluator；以上
属于公开基线对比，不是官方 Docker 环境的逐字节复现。

结构化结果与原始哈希：

- `benchmark/results/mineru_vlm_accuracy_20260825.json` / `.md`
- `benchmark/results/pipeline_v2_accuracy_20260825.json` / `.md`
- `benchmark/results/pipeline_comparison_20260825.json` / `.md`
- `benchmark/OmniDocBench/result/architecture-v2-20260821_quick_match_run_summary.json`
- `benchmark/OmniDocBench/result/pipeline-v2-20260825_quick_match_run_summary.json`
- `benchmark/OmniDocBench/result/mineru-3.4.5-pipeline-20260825_quick_match_run_summary.json`
- `benchmark/OmniDocBench/result/uparser-pipeline-v2-mineru345-ppformula-20260826_quick_match_run_summary.json`
- `benchmark/OmniDocBench/result/uparser-pipeline-v2-mineru345-ppformula-20260826_quick_match_metric_result.json`
- `benchmark/omnidoc_pred/uparser-pipeline-v2-rust-cli-staged-20260826/summary.json`
- `benchmark/OmniDocBench/result/uparser-pipeline-v2-rust-cli-staged-20260826_quick_match_run_summary.json`
- `benchmark/OmniDocBench/result/uparser-pipeline-v2-rust-cli-staged-20260826_quick_match_metric_result.json`

### 1.4 Qwen3.8-27B 历史实验解读

**解读**:
- Text Edit、Reading Order Edit 均优于官方 235B 通用 VLM 参考——对一个约 27B、未经文档解析任务微调的通用对话模型而言相当亮眼。
- Table TEDS(0.7920)比端口混用时的旧结果(0.7161)高出 **+0.076**,证实之前的分数确实被 Qwen3.5-4B 拖累(该后端单独的 Table TEDS 约 0.53,见 `BENCHMARK_DEV_LOG.md` §2.4);纯净后仍略落后于 mineru-vlm 参照和官方 Qwen3-VL-235B,但差距明显收窄——通用对话 VLM 在无版面检测训练下,复杂表格结构还原仍是相对短板,但没有之前看起来那么严重。
- Formula Edit 反而变差(0.1373→0.1614)——纯 27B 承担了此前被 4B 分走的那部分请求,含更多复杂公式页面,不是模型能力倒退,是样本分布变化;该列本就因缺 CDM 环境不可与官方列直接对比,仅作内部相对参照。

### 复现

```bash
cd benchmark
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
python3 run_uparser_omnidoc.py --name architecture-v2-20260821 --protocol mineru-vlm --workers 4
python3 summarize_omnidoc.py architecture-v2-20260821

python3 gen_qwen_omnidoc.py --name qwen3.8-27b-pure --workers 6          # 生成 1651 页预测(端口 8094,默认值)
python3 gen_qwen_omnidoc.py --name qwen3.8-27b-pure --skip-generate      # 仅跑官方评测器
python3 summarize_omnidoc.py qwen3.8-27b-pure                            # 汇总四项指标
```

`pipeline_v2.rs` Rust CLI staged 复现命令：

```bash
cargo build --manifest-path uparser/Cargo.toml -p uparser-core --release --features pdfium

python3 benchmark/run_pipeline_v2_benchmarks.py opendataloader --runner cli --workers 1 \
  --endpoint http://127.0.0.1:19001 \
  --output opensource/opendataloader-bench/prediction/uparser-pipeline-v2-rust-cli-staged-20260826
(cd opensource/opendataloader-bench && \
  .venv/bin/python src/evaluator.py --engine uparser-pipeline-v2-rust-cli-staged-20260826 --log-level INFO)

python3 benchmark/run_pipeline_v2_benchmarks.py omnidoc --runner cli --workers 1 \
  --endpoint http://127.0.0.1:19001 \
  --output benchmark/omnidoc_pred/uparser-pipeline-v2-rust-cli-staged-20260826
python3 benchmark/run_uparser_omnidoc.py \
  --name uparser-pipeline-v2-rust-cli-staged-20260826 --protocol pipeline --skip-generate
```

上述服务用 `UPARSER_PIPELINE_PROFILE=legacy`、`UPARSER_PIPELINE_DEVICE=cuda` 和
`UPARSER_PIPELINE_MAX_IMAGE_PIXELS=200000000` 启动，Omni 评分额外使用报告产物
`*_runtime_environment.json` 记录的 TeX Live 2022/CJK 环境。生产服务仍应保留默认
5,000 万像素上限。

当前 V2 官方结果的可提交副本为 `benchmark/results/architecture_v2_omnidoc_metric_20260821.json` 和
`benchmark/results/architecture_v2_omnidoc_summary_20260821.json`；完整方法与哈希见
`ARCHITECTURE_V2.0_EVALUATION_REPORT.md`。

---

## 2. Prompt 改进实验结论:失败,不采纳(混用端点与纯净端点上重复验证过,结论一致)

在官方基线 prompt 上测试了增量变体(表格结构化指令 B、多栏阅读顺序 C、组合 BC),290 页分层子集上都显著提升 Table TEDS,但**两次独立的全量 1651 页确认跑都显示子集判断不成立**——先在混用端点(`127.0.0.1:8087`,已废弃,见 `BENCHMARK_DEV_LOG.md` §2.4)上跑过一轮,后在迁移到专用端口 8094(纯净、无端点混用问题)后又重跑了一轮变体 B,两次结论方向一致:

| | 子集(290页)Table TEDS | 全量(1651页)Table TEDS | 全量结论 |
|---|---|---|---|
| 混用端点·变体 B | 0.6781→0.7399(+0.062) | 0.7161→0.6827(**-0.033**) | 反转,净负 |
| 混用端点·变体 BC | 0.6781→0.7731(+0.095) | 0.7161→0.6668(**-0.049**) | 反转,净负 |
| **纯净端点·变体 B** | 0.8280→0.8609(+0.033) | 0.7920→0.7876(**-0.0044**) | 打平略负 |

根因两次一致:新增指令在真正困难的类目(`layout_hard`、`table_hard`)上确实让模型更仔细、分数上涨,但在本来就简单规整的类目(`watermark`/`fuzzy_scan`/`magazine` 等)上让模型对规整表格过度分析、反而做坏——子集抽样的分层标签没有覆盖到这些受害类目,导致子集判断方向性错误。纯净端点上模型本身更强,"做坏"的幅度小了很多(不再像混用端点时 `watermark` 类目暴跌 0.53),但简单类目的损失量级仍然和难例类目的收益量级相当,net 结果打平偏负,**不构成一个值得采纳的改进**。

**结论(在混用端点和纯净端点上都成立):Qwen3.8-27B 在 OmniDocBench 上继续用官方基线 prompt(§1),不采纳任何测试过的变体(B/C/BC)。** 完整实验过程、门槛判定、后端拆分复核、逐类目诊断表、纯净端点复测细节、复现命令见 `BENCHMARK_DEV_LOG.md` §3;实验方案见 `QWEN_PROMPT_IMPROVEMENT_PLAN.md`。

---

## 3. 2026-09-09 统一复测:公式渲染缺陷的发现、修复与全量复跑

> 语料与评测器同 §1（全量 1651 页、官方 `run_eval.py` `quick_match`、CDM 8 workers、
> TEDS 24 workers、match 24 workers）。预测全部经 `uparser` CLI 产出，由
> `benchmark/run_dual_benchmark.py` 驱动。`native` 不在本 Part 评测：它是零模型纯文本层
> 引擎，本榜单输入是页面图像，没有可读文本层，跑出来只会是空——不为凑满表格而报一个
> 注定为 0 的格子。

### 3.1 结果（修复后）

| 模式 | Overall↑ | Text Edit↓ | Formula CDM↑ | Table TEDS↑ | TEDS-S↑ | Order Edit↓ | s/页 |
|---|---:|---:|---:|---:|---:|---:|---:|
| **uparser · mineru-vlm** | **91.3490** | 0.083655 | **92.1094** | 90.3032 | 93.8022 | 0.144774 | 1.223 |
| **uparser · pipeline** | **88.3451** | 0.070588 | **91.1163** | 80.9780 | 88.1804 | 0.153009 | 2.372 |
| 官方 MinerU2.5-Pro（参照） | 95.75 | 0.036 | 97.45 | 93.42 | 95.92 | 0.120 | — |
| 官方 MinerU-Pipeline（参照） | 86.47 | 0.055 | 83.07 | 81.88 | 88.68 | 0.153 | — |

两条路径都是 1651/1651 生成成功、0 失败。

### 3.2 修复前后 A/B

同一语料、同一 vLLM 端点、同一 bare tensor 服务，只差这次的渲染修复：

| | Overall | Formula CDM | Text Edit↓ | Table TEDS | Order Edit↓ |
|---|---:|---:|---:|---:|---:|
| mineru-vlm 修复前 | 74.0004 | 40.1003 | 0.084857 | 90.3864 | 0.148516 |
| mineru-vlm 修复后 | **91.3490** | **92.1094** | 0.083655 | 90.3032 | 0.144774 |
| pipeline 修复前 | 84.3639 | 79.2622 | 0.071484 | 80.9780 | 0.162814 |
| pipeline 修复后 | **88.3451** | **91.1163** | 0.070588 | 80.9780 | 0.153009 |

收益集中在 Formula CDM（`+52.01` / `+11.85`）。表格分几乎不动——pipeline 逐位相同
（`80.977978` 两次一致），mineru-vlm 仅差 `-0.083`，属 VLM 采样非确定性而非改动影响；
文本与阅读顺序小幅改善。整体与"只改了公式渲染路径"这一事实一致。修复后两者的 CDM
都**高于本报告此前的历史最好值**（§1.2 的 `88.8433` / `88.4867`）。

### 3.3 缺陷本身

根因是 canonical 模型**没有块级公式表达**：`ascend.rs` 把 `Block.latex` 降级成一个自带
定界符的**文本段落**，于是 Markdown 渲染器按散文转义它，`\`、`[`、`]` 全部被加反斜杠：

```
$$
\\\[\boldsymbol {A} \boldsymbol {B} = \left\[ \begin{array}{c c} 2 & 3 \\\ 1 & 4 \end{array} \right\] ...\\\]
$$
```

三个独立表现：

1. **LaTeX 被当散文转义** —— `\left[`→`\left\[`、`\\`→`\\\`。mineru-vlm 与 pipeline 同时中招。
2. **display math 被包两层** —— 适配器侧 `formula_repair::wrap_display_math` 已经加了
   `\[…\]`（D.10），渲染器再加一层 `$$…$$`，得到 `$$ \[ … \] $$`，没有任何
   Markdown+KaTeX 渲染器会把它读成一个公式。仅 mineru-vlm（dots-ocr/MonkeyOCRv2 同类）。
3. **行内公式被转义** —— 适配器把 `$a_{kj}$` 直接拼进 `Block.text`（`pipeline_v2` 的
   inline-formula 装配），渲染器把它当散文，输出 `$a\_{k j}$`。修复前
   **435/1651（mineru-vlm）和 283/1651（pipeline）页**含被转义的 `\_`。

修复（4 处，均在渲染链路，未改任何模型/适配器推理逻辑）：

- `uparser-document-engine/src/model.rs`：新增 `Block::Formula { source, display }`。
- `uparser-document-engine/src/render/mod.rs`：该块**逐字**输出，不经转义器。
- `uparser-core/src/formula_repair.rs`：新增 `strip_display_math()`，剥掉适配器自带的
  一层定界符。保守实现——剥完后正文若仍含闭定界符就不剥，所以 `$a$ + $b$`
  这种"两个公式"不会被误当成"一个被包裹的公式"。
- `uparser-core/src/ascend.rs`：改用上述两者；新增 `split_inline_math()` 把行内公式
  拆成 `Inline::Formula`。护栏与损害对齐——只有 `$…$` 内含转义器真正会破坏的字符
  （`\ ^ _ {`）时才判定为数学，因此散文金额 `$5 to $10` 原样保留（有专门测试）。

**为什么长期没被发现**：ODL 榜单不评公式（Part A §7 结论 4，修复前后 Overall 几乎不变），
而单元测试测的是 `wrap_display_math`/`escape_inline_text` 各自的行为，没有一条测试
把"适配器产出的 latex"走完整条渲染链。已补 3 条端到端回归测试
（`latex_survives_rendering_without_markdown_escaping`、
`an_adapter_that_pre_wrapped_its_latex_is_not_wrapped_twice`、
`inline_math_glued_into_text_is_not_escaped_as_prose`），全部先确认能复现旧行为再修。

### 3.4 复现

```bash
# 1) bare tensor 模型服务（pipeline 依赖；conda base 的 numpy/scipy ABI 不匹配，
#    且 transformers 5.9.0 超出服务要求的 >=4.57.3,<5.0.0，必须用 minerUEnv）
CUDA_VISIBLE_DEVICES=1 PYTHONPATH=services/pipeline-model-server/src \
UPARSER_PIPELINE_DEVICE=cuda UPARSER_PIPELINE_PROFILE=mineru-3.4.5 \
UPARSER_MINERU_ROOT=opensource/MinerU UPARSER_MINERU_CONFIG=/home/dataset1/gaojing/mineru.json \
UPARSER_PIPELINE_MAX_IMAGE_PIXELS=200000000 UPARSER_PIPELINE_PORT=19001 \
~/anaconda3/envs/minerUEnv/bin/python -m uparser_pipeline_server.bare_app

# 2) 二进制
cargo build --manifest-path uparser/Cargo.toml -p uparser-core --release --features pdfium,native

# 3) 两个榜单 × 三种模式，生成 + 官方评测 + 汇总
python3 benchmark/run_dual_benchmark.py --tag 20260909 --force
```

产物：`benchmark/results/dual_benchmark_20260909.{json,md}`（修复前的对照留在
`dual_benchmark_20260908.*`），预测在
`benchmark/omnidoc_pred/uparser-{mineru-vlm,pipeline}-omnidoc-20260909/` 与
`opensource/opendataloader-bench/prediction/uparser-*-odl-20260909/`。

### 3.5 局限

- **CDM 依赖的 TeX 环境在本机原本不存在**：`pdflatex` 解析到不存在的
  `/share/texlive/pdflatex`，CDM 对**完全相同的公式也返回 0.0**——不修的话本节所有
  CDM 与 Overall 都是错的，且不会有任何报错。本轮安装了 TeX Live 2026
  （`/home/dataset1/gaojing/texlive/2026`，scheme-small + `was/multirow/cjk-ko`），
  先验证 `identical=1.0 / different=0.0 / 中文=1.0` 才开跑，并把路径固化进脚本的
  `--texlive-root`（缺失时打警告，不静默出 0）。与 §1 历史行使用的 TeX Live 2022
  不是同一套渲染环境，CDM 的跨日期比较应视为近似。
- **两条路径在同样的 2 页上输出为空**（一本英语教材的纯图页）。两个独立协议同页同结果，
  指向该页本身没有文本内容，而非单协议缺陷。
- mineru-vlm 的 Table TEDS `90.3032` 比 §1.2 历史的 `92.4279` 低约 2 分；该差距**在修复
  前后基本一致**（`90.3864` vs `90.3032`，相差 `0.083`），与本次改动无关，属
  checkpoint/配置差异。
- 速度栏是并发吞吐（mineru-vlm 4 workers、pipeline 8 workers），不是单页延迟。

---

# Part C:大文档 OCR Pipeline 性能与稳定性

> 测试日期：2026-09-02。本 Part 没有标注真值，**不是精度榜单**，不能用于证明任一路径的 OCR 精度更高。

## 1. 语料与配置

- 输入：`/home/dataset1/gaojing/datasets/pdf/all_pdf/PDFs/data/黄陂污水治理项目东北片区报告书上册.pdf`
- 规模：544 页，45,119,340 bytes。
- 两路径共同参数：200 DPI、禁用缓存、禁用资产写出、release `uparser`。
- Rust Pipeline V2：窗口 16，最大并发 4；Rust 负责 layout→OCR→table→assemble 编排，外部服务只提供裸模推理。
- MinerU-VLM：`MinerU2.5-Pro-2605-1.2B`，窗口 1024，客户端最大并发 1024。
- 计时：`/usr/bin/time`端到端墙钟，包含 PDF 栅格化、模型请求、Rust 后处理和 Markdown 写出。服务均已预热。

## 2. 完整结果

| 指标 | Rust Pipeline V2 | MinerU-VLM c1024 |
|---|---:|---:|
| 成功页 | 544/544 | 544/544 |
| 退出码 | 0 | 0 |
| 墙钟时间 | 1,258.80 s | 291.77 s |
| 每页耗时 | 2.3140 s | 0.5363 s |
| 吞吐 | 0.4322 page/s | 1.8645 page/s |
| 客户端峰值 RSS | 16.360 GiB | 29.998 GiB |
| Markdown 大小 | 1,898,817 bytes | 1,587,318 bytes |
| Markdown 行数 | 20,727 | 7,843 |
| Markdown 标题 | 497 | 340 |
| HTML 表格 | 514 | 441 |
| SHA-256 | `c6def7dcc3933ada8dfc97590cb4a6adb7aa015445b0c0eb804894ad7b458a31` | `25c976a83cedc34a1d57192e7b0872231e4b83f4cdd61242c06afe0f131786b7` |

在这两个**当前运行 profile**下，MinerU-VLM 的墙钟速度是 Pipeline V2 的
`4.314×`，耗时减少 `76.822%`；代价是客户端峰值 RSS 从 `16.360 GiB`
增至 `29.998 GiB`。两路径的并发分别为 4 和 1024，因此该 `4.314×` 是实际配置下的端到端结果，**不是单请求模型速度的严格同并发 A/B**。

Markdown 大小、行数、标题数和表格数只用于证明输出非空并呈现结构差异。由于该 PDF 无真值，不得从这些数量推导召回率、TEDS、标题精度或“超过 MinerU 3.4.5”。后者仍以 Part A/B 的 ODL 和 OmniDocBench 官方指标为准。

## 3. 1024 并发边界

`uparser --max-concurrency 1024` 是全文档共享的 HTTP 在途请求许可预算，调度窗口也已提升为 1024；该配置下 544/544 页完成。但当前 `:19122` vLLM 启动命令未显式设置 `--max-num-seqs`。已安装 vLLM 在小于 70 GiB GPU 上的 OpenAI API Server 默认值为 `256`，所以本轮证明的是“1024 客户端请求预算下稳定完成”，不是“模型引擎同时执行 1024 序列”。

因此，“客户端并发配置 `>=1000`”已通过；“模型引擎执行并发 `>=1000`”当前为 **FAIL**。要验证后者，需以 `--max-num-seqs 1024` 重启服务，并使用至少 1024 个可同时产生请求的工作单元采样 `vllm:num_requests_running`峰值。单份 544 页 PDF 的第一阶段最多只能产生 544 个页级请求。

## 4. 产物与复现

- Pipeline Markdown：`output/黄陂污水治理项目东北片区报告书上册.pipeline-benchmark.md`
- Pipeline 计时：`output/黄陂污水治理项目东北片区报告书上册.pipeline-timing.txt`
- MinerU-VLM Markdown：`output/黄陂污水治理项目东北片区报告书上册.mineru-vlm-c1024.md`
- MinerU-VLM 计时：`output/黄陂污水治理项目东北片区报告书上册.mineru-vlm-c1024-timing.txt`
- 独立详细报告：`output/黄陂污水治理项目东北片区报告书上册.performance-comparison.md`
- 无 Python 复测脚本：`benchmark/run_mineru_vlm_pdf_benchmark.sh`；脚本强制要求客户端并发和声明的服务端 `max-num-seqs` 均不小于 1000，否则拒绝运行。
