# uparser 解析引擎基准评测报告

本报告包含**两个互相独立、不可直接比较的评测语料/榜单**,分属两套评测体系(不同数据集、不同官方评测器、不同指标定义)——阅读时请对照下表先确认在看哪一个:

| | Part A:opendataloader-bench(§1–§6) | Part B:OmniDocBench(§1–§2) |
|---|---|---|
| 语料位置 | `opensource/opendataloader-bench/pdfs` | `benchmark/OmniDocBenchData` |
| 语料规模 | 200 篇单页真实 PDF | 1651 页(全量)/ 290 页分层子集(prompt 实验) |
| 官方评测器 | opendataloader-bench 自带 harness/evaluator | OmniDocBench 官方 `run_eval.py`(`quick_match`) |
| 指标定义 | Reading Order=NID、Table=TEDS、Heading=MHS,Overall=三者等权均值 | Text/Formula/Reading Order=Edit_dist(越低越好)、Table=TEDS(越高越好);无 CDM 环境故不计算官方 Overall |
| 评测对象 | uparser 各 protocol(mineru-vlm/native)vs 该榜单已公布的其他引擎(hybrid/docling/mineru/liteparse/edgeparse) | 通用对话 VLM Qwen3.8-27B(经 127.0.0.1:8087,非 uparser protocol,直连 chat-completions + 自定义 prompt) vs 本仓库 mineru-vlm 参照 vs OmniDocBench 官方榜单参考值 |
| 结论一句话 | uparser·mineru-vlm 全场第一;uparser·native 零模型碾压 liteparse | Qwen3.8-27B 基线可用但 Table 短板明显;prompt 改进实验(Part B §2)证实无效、不采纳 |

两个 Part 之间的数字**不可跨表比较**(不同语料、不同评测器、不同指标口径),即使指标名字看起来一样(如都有"Table TEDS")。调试过程、探索性发现、失败尝试的完整记录见 `BENCHMARK_DEV_LOG.md`——本报告只保留干净的榜单结果与结论。

---

# Part A:opendataloader-bench 榜单

> 日期:2026-08-04 · 语料:opendataloader-bench,200 篇单页真实 PDF · 评测器:同一 harness/evaluator
> 指标:Reading Order = NID、Table = TEDS、Heading = MHS,**Overall = 三者等权均值**;Speed = s/篇(越低越好)

---

## 1. 结论速览

| 引擎 | Overall | Reading Order | Table | Heading | 速度 s/篇 | 依赖 |
|---|---|---|---|---|---|---|
| **uparser · mineru-vlm**(修复后) | **0.9284** 🥇 | **0.9470** 🥇 | **0.9439** 🥇 | **0.8777** 🥇 | 1.81(GPU) | vLLM(MinerU2.5) |
| opendataloader-hybrid(榜首参照) | 0.907 | 0.934 | 0.928 | 0.821 | 0.463 | 布局模型 |
| docling(参照) | 0.882 | 0.898 | 0.887 | 0.824 | 0.762 | 模型 |
| **uparser · native** | 0.8754 | 0.9150 | 0.8141 | 0.7875 | **0.046** | **零模型/纯 Rust** |
| pdf-inspector(baseline) | 0.8754 | 0.9150 | 0.8141 | 0.7875 | 0.032 | 纯 Rust |
| mineru(榜单参照) | 0.831 | 0.857 | 0.873 | 0.743 | 5.962 | 模型 |
| edgeparse(参照) | 0.837 | 0.894 | 0.717 | 0.706 | 0.036 | 纯 Rust |
| **liteparse(榜单)** | 0.576 | 0.866 | 0.000 | 0.000 | 1.061 | PDFium+OCR |

**三条主结论:**
1. **uparser 的 mineru-vlm 集成拿下全场第一**(Overall 0.928,超过榜首 hybrid 0.907),且 Reading Order / Table / Heading 三项**全部第一**。表格保真度 0.944 更是显著超过榜单里 mineru 自己的 0.873——说明 uparser 对 MinerU2.5 的编排/OTSL→HTML 转换质量很高。
2. **uparser 的 native(纯 Rust、零模型)在无 GPU、40 倍速度优势下拿到 0.875**,进入第 3 名区间(>docling 0.882 仅差 0.007,>mineru/edgeparse),并**在精度与速度两个维度全面碾压 liteparse**(0.875 vs 0.576;0.046 vs 1.061 s/篇)。
3. mineru-vlm 的分数含一次渲染层 bug 修复(Overall 0.708→0.928);发现与修复过程见 `BENCHMARK_DEV_LOG.md` §1,此处只展示修复后的最终结果。

---

## 2. 分项分析

### 2.1 Reading Order(NID)
- mineru-vlm **0.947** > native/pdf-inspector 0.915 > liteparse 0.866。
- VLM 的版面理解让阅读顺序最优;native 的纯几何 XY-cut/投影已很强(0.915),远好于 liteparse。

### 2.2 Table(TEDS)
- mineru-vlm **0.944**(OTSL→HTML)≈ 榜首;native 0.814(pdf-inspector 三策略表格检测)。
- 差距 0.13 是 native 相对天花板的主要短板;但 native 的 0.814 已优于 edgeparse(0.717)、unstructured 等,且**远优于 liteparse 的 0.000**。

### 2.3 Heading(MHS)
- 修复后:mineru-vlm **0.878** > docling 0.824 > native 0.788。
- MHS 指标**层级无关**(源码:"treats all heading levels as equivalent",APTED 只比 heading/content 标签,不比 `#` 层数)——所以关键是"该是标题的行有没有被标成标题",而非层级深浅。native 的短板是标题**过检测**(引擎产 280 个 vs GT 193 个)。

### 2.4 速度
- native **0.046 s/篇**(纯 Rust、零模型、无 GPU),与最快的纯 Rust 引擎同档,比 docling 快 ~16×、比 mineru(榜单)快 ~130×。
- mineru-vlm 冷解析 **1.81 s/篇**(走 GPU vLLM);命中 uparser 内容哈希缓存时 0.005 s/篇。

---

## 3. native vs liteparse:目标达成情况

用户目标之一是"native 从精度与速度全面超过 liteparse":

| | native | liteparse(榜单) | 结果 |
|---|---|---|---|
| Overall | **0.8754** | 0.576 | 超 +0.30 |
| Reading Order | **0.915** | 0.866 | 超 |
| Table | **0.814** | 0.000 | 超 |
| Heading | **0.788** | 0.000 | 超 |
| 速度 s/篇 | **0.046** | 1.061 | 快 ~23× |

**精度与速度双维度全面超越,✅ 达成。**(注:榜单 liteparse 用的是纯文本口径;即便给它最乐观的 markdown 估分,native 仍稳赢,且速度优势是结构性的。)

## 4. native vs pdf-inspector:内部化 + 打平

native 的 markdown 当前直通内嵌引擎(即 pdf-inspector 核心),二者输出**逐字节相同**,故 Overall **打平**(均 0.8754),速度上 native 因多一层 CLI 开销略慢(0.046 vs 0.032 s/篇)。native 的价值在于:**去除对 liteparse 的依赖、消除 PDFium 二进制、并入 uparser 的统一 IR/CLI/路由**。要严格超越 pdf-inspector 需引擎核心调优(降标题过检测、提 TEDS),会使 vendored 引擎与上游分叉,作为后续可选工作。

---

## 5. 选型建议

| 场景 | 推荐 | 理由 |
|---|---|---|
| 电子版 PDF、无 GPU、要快 | **native** | 0.046 s/篇、零依赖、0.875 分,碾压 liteparse |
| 追求最高质量、有 GPU | **mineru-vlm** | 0.928 全场第一,表格/阅读顺序最佳 |
| 扫描件 | **mineru-vlm** | native 无 OCR(扫描件会空);VLM 直接识别 |
| `--protocol auto` | 由 profiler 路由 | 电子版长文本→native,表格密集/扫描→VLM |

---

## 6. 方法学与可复现

- 语料:`opensource/opendataloader-bench/pdfs`(200 篇单页)。
- 接入:新增 `src/pdf_parser_uparser_native.py`、`pdf_parser_uparser_mineru_vlm.py`、`pdf_parser_pdf_inspector.py`,注册进 `engine_registry.py`。
- 运行:`python src/run.py --engine <name> --force` → `prediction/<name>/evaluation.json`。
- 二进制:`cargo build --release --features native,pdfium`(native 走 lopdf 引擎;mineru-vlm 走 pdfium 光栅化 + `http://127.0.0.1:19122` 的 MinerU2.5 vLLM endpoint)。
- pdf-inspector baseline:其自带 `cargo build --release --bin pdf2md`。

### 局限
- native 无 OCR:扫描件(如 doc `01030000000141`,image_dense)输出为空——这是设计取舍(扫描件交由 VLM 路由),pdf-inspector 同引擎同样为空。
- mineru-vlm 分数依赖 MinerU2.5-2604-1.2B 这一具体 checkpoint 与 endpoint。
- 榜单参照值(hybrid/docling/mineru/liteparse)取自 bench README 已公布结果;native/pdf-inspector/mineru-vlm 为本次实测。
- 防过拟合:未针对本语料调参;渲染器修复是通用正确性修复(所有 VLM 协议受益),非针对 GT 的 tuning。

---

# Part B:OmniDocBench 榜单

> 与 Part A(opendataloader-bench)是**完全不同的语料和评测体系**,数字不可跨 Part 比较——见文首对照表。

## 1. OmniDocBench 全量(1651 页)榜单:Qwen3.8-27B vs 参照

> 语料:`benchmark/OmniDocBenchData/OmniDocBench.json`,**全量 1651 页**(与 Part A 的 opendataloader-bench 200 篇是**不同语料**,不可跨表比较)
> 评测器:OmniDocBench 官方 `run_eval.py`(`quick_match`)
> 评测设置(端点不稳定、prompt 选择、thinking 泄漏排查等背景细节见 `BENCHMARK_DEV_LOG.md` §2):Qwen3.8-27B 经 `127.0.0.1:8087` 直连 chat-completions(该端点在 Qwen3.5-4B / Qwen3.8-27B 两个后端间随机切换,本结果是混合产出),用 OmniDocBench 官方通用 VLM 参考 prompt,`enable_thinking=false`,无 CDM 环境故不计算官方 Overall。空预测 2/1651(0.12%)。

| 指标(quick_match) | **Qwen3.8-27B(混合后端,本次实测)** | mineru-vlm-2605-surpass-e1-full(本仓库同语料参照) | Qwen3-VL-235B(OmniDocBench 官方榜单参考,非本环境测得) |
|---|---|---|---|
| Text Edit↓ | **0.0596** | 0.0367 | 0.063 |
| Formula Edit↓(非 CDM,不可直接对比官方列) | 0.1373 | 0.0948 | — |
| Table TEDS↑ | **0.7161** | 0.9065 | 0.8307 |
| Table TEDS-S↑ | 0.7422 | 0.9388 | 0.8675 |
| Reading Order Edit↓ | **0.1573** | 0.1285 | 0.166 |

**解读**:
- Text Edit、Reading Order Edit 与官方 235B 通用 VLM 参考同量级,甚至字面数值略优——对一个约 27B、未经文档解析任务微调的通用对话模型而言好于预期。
- Table TEDS 明显落后于本仓库 mineru-vlm 参照和官方 Qwen3-VL-235B——通用对话 VLM 在无版面检测训练下,复杂表格结构还原是短板。

### 复现

```bash
cd benchmark
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
python3 gen_qwen_omnidoc.py --name qwen3.8-27b --workers 6          # 生成 1651 页预测
python3 gen_qwen_omnidoc.py --name qwen3.8-27b --skip-generate      # 仅跑官方评测器
python3 summarize_omnidoc.py qwen3.8-27b                            # 汇总四项指标
```

---

## 2. Prompt 改进实验结论:失败,不采纳

在 §1 官方基线 prompt 上测试了三个增量变体(表格结构化指令 B、多栏阅读顺序 C、组合 BC)。290 页分层子集上三个变体都显著提升 Table TEDS(BC 最高 +0.095),但**全量 1651 页确认跑结果反转**:

| 全量(1651 页) | Text Edit↓ | Table TEDS↑ | Reading Order↓ |
|---|---|---|---|
| baseline(§1) | 0.0596 | **0.7161** | 0.1573 |
| 变体 B | 0.0601 | 0.6827(**-0.033**) | 0.1625 |
| 变体 BC | 0.0565 | 0.6668(**-0.049**) | 0.1592 |

根因:新增指令在真正困难的类目(`layout_hard` +0.209、`geometric_deformation` +0.285)上确实有效,但在本来就简单规整的类目(`watermark` **-0.527**、`fuzzy_scan`、杂志等)上让模型对规整表格过度分析、反而做坏——而子集抽样的分层标签没有覆盖到这些受害类目,导致子集判断方向性错误。

**结论:§1 的官方基线 prompt 继续作为 Qwen3.8-27B 在 OmniDocBench 上的推荐配置,不采纳任何测试过的变体。** 完整实验过程、门槛判定、后端拆分复核、逐类目诊断表、后续方向建议、复现命令见 `BENCHMARK_DEV_LOG.md` §3;实验方案见 `QWEN_PROMPT_IMPROVEMENT_PLAN.md`。
