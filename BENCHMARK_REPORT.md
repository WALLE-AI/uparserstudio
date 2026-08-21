# uparser 解析引擎基准评测报告

本报告包含**两个互相独立、不可直接比较的评测语料/榜单**,分属两套评测体系(不同数据集、不同官方评测器、不同指标定义)——阅读时请对照下表先确认在看哪一个:

| | Part A:opendataloader-bench(§1–§6) | Part B:OmniDocBench(§1–§2) |
|---|---|---|
| 语料位置 | `opensource/opendataloader-bench/pdfs` | `benchmark/OmniDocBenchData` |
| 语料规模 | 200 篇单页真实 PDF | 1651 页(全量)/ 290 页分层子集(prompt 实验) |
| 官方评测器 | opendataloader-bench 自带 harness/evaluator | OmniDocBench 官方 `run_eval.py`(`quick_match`) |
| 指标定义 | Reading Order=NID、Table=TEDS、Heading=MHS,Overall=三者等权均值 | Text/Formula/Reading Order=Edit_dist(越低越好)、Table=TEDS(越高越好);无 CDM 环境故不计算官方 Overall |
| 评测对象 | uparser V2 的 mineru-vlm/native/auto、历史冻结结果与该榜单已公布的其他引擎(hybrid/docling/mineru/liteparse/edgeparse) | 当前 uparser V2 mineru-vlm、历史 mineru-vlm、通用对话 VLM Qwen3.8-27B 与 OmniDocBench 官方榜单参考值 |
| 结论一句话 | 当前 V2 mineru-vlm 仍超过外部榜首参照，native 保真，auto 给出质量/速度折中 | 当前 V2 全量运行稳定，但相对历史 surpass/official 质量有回退，不能宣称超过现有最佳基线 |

两个 Part 之间的数字**不可跨表比较**(不同语料、不同评测器、不同指标口径),即使指标名字看起来一样(如都有"Table TEDS")。调试过程、探索性发现、失败尝试的完整记录见 `BENCHMARK_DEV_LOG.md`——本报告只保留干净的榜单结果与结论。

---

# Part A:opendataloader-bench 榜单

> 原始榜单日期:2026-08-04；V2 复测日期:2026-08-21 · 语料:opendataloader-bench,200 篇单页真实 PDF · 评测器:同一 harness/evaluator
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
| mineru(榜单参照) | 0.831 | 0.857 | 0.873 | 0.743 | 5.962 | 模型 |
| edgeparse(参照) | 0.837 | 0.894 | 0.717 | 0.706 | 0.036 | 纯 Rust |
| **liteparse(榜单)** | 0.576 | 0.866 | 0.000 | 0.000 | 1.061 | PDFium+OCR |

**三条主结论:**
1. **当前 V2 mineru-vlm 仍超过外部榜首 hybrid**(Overall 0.924 vs 0.907)，并以 Table TEDS `0.9682` 超过历史冻结结果 `0.9439`；但 Overall、Reading Order 和 Heading 分别回退 `0.0044`、`0.0037`、`0.0105`。回退均小于 V2 发布闸门 `0.02`，但不能把历史 `0.9284` 写成当前 V2 分数。
2. **V2 native 与冻结输出和质量逐文件一致**，统一 runner 的无缓存开销从 `0.0473` 增至 `0.0508 s/篇`，约 `+7.33%`；仍在精度与速度两个维度超过 liteparse。
3. **V2 auto 是折中档**：156 篇走 native、44 篇走 mineru-vlm，Overall `0.8920`，比纯 native 高 `0.0166`，速度约为全量 VLM 的 `4.53x`。
4. 历史 mineru-vlm 分数含一次渲染层 bug 修复(Overall 0.708→0.928);发现与修复过程见 `BENCHMARK_DEV_LOG.md` §1。当前 V2 使用不同 checkpoint，历史与当前比较是发布回归门槛，不是纯框架微基准。

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

native 的 Markdown 当前直通内嵌引擎(即 pdf-inspector 核心)，V2 与冻结输出**逐字节相同**，故 Overall **打平**(均 0.8754)。V2 统一 runner 无缓存速度为 `0.0508 s/篇`，冻结 native 为 `0.0473 s/篇`，独立 pdf-inspector baseline 为 `0.032 s/篇`。native 的价值在于去除 liteparse 依赖、消除 PDFium 二进制并纳入统一 IR/CLI/路由；要严格超越 pdf-inspector 仍需引擎核心调优。

---

## 5. 选型建议

| 场景 | 推荐 | 理由 |
|---|---|---|
| 电子版 PDF、无 GPU、要快 | **native** | 0.0508 s/篇、零依赖、0.875 分，明显超过 liteparse |
| 追求当前 V2 最高质量、有 GPU | **mineru-vlm** | Bench A Overall 0.924、Table TEDS 0.968；OmniDocBench 尚未超过历史最佳 |
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
- 当前 V2 mineru-vlm 分数依赖 `MinerU2.5-Pro-2605-1.2B`；历史冻结结果使用更早 checkpoint/configuration，不能当作纯框架 A/B。
- 榜单参照值(hybrid/docling/mineru/liteparse)取自 bench README；V2 native/mineru-vlm/auto 与历史冻结结果均为本地实测，但运行日期和模型配置不同。
- 防过拟合:未针对本语料调参;渲染器修复是通用正确性修复(所有 VLM 协议受益),非针对 GT 的 tuning。

---

# Part B:OmniDocBench 榜单

> 与 Part A(opendataloader-bench)是**完全不同的语料和评测体系**,数字不可跨 Part 比较——见文首对照表。

## 1. OmniDocBench 全量(1651 页)榜单:当前 V2 vs 历史与外部参照

> 语料:`benchmark/OmniDocBenchData/OmniDocBench.json`,**全量 1651 页**(与 Part A 的 opendataloader-bench 200 篇是**不同语料**,不可跨表比较)
> 评测器:OmniDocBench 官方 `run_eval.py`(`quick_match`)
> 评测设置:Qwen3.8-27B 经 `127.0.0.1:8094` 直连 chat-completions,用 OmniDocBench 官方通用 VLM 参考 prompt,`enable_thinking=false`,无 CDM 环境故不计算官方 Overall。空预测 17/1651(1.0%,均为 180s 读超时,该端点无并发优化)。**这是纯净结果**——首轮测试用的 `127.0.0.1:8087` 被发现同时挂了两个独立 vLLM 进程(Qwen3.5-4B 与 Qwen3.8-27B 通过 `SO_REUSEPORT` 共享同一端口,内核在两者间随机分发请求),导致结果混入了 Qwen3.5-4B;服务已迁移到专用端口 8094 并验证port-clean,本表为重测后的纯 Qwen3.8-27B 结果。混合结果与排查过程见 `BENCHMARK_DEV_LOG.md` §2.4。

### 1.1 当前 V2 全量复测（2026-08-21）

当前 release 使用 `mineru-vlm` 协议、`MinerU2.5-Pro-2605-1.2B`、4 个生成 worker 和 `--no-cache`。
1,651/1,651 页均生成成功，非零返回码 0；其中 1 页 Markdown 仅含换行。官方 page match 无 timeout
fallback，665 个 TEDS 样本无 timeout/error/exception。

| 指标(quick_match) | **当前 uparser V2** | 历史 mineru-vlm-2605-surpass-e1-full | **Qwen3.8-27B(纯净实测)** | Qwen3-VL-235B(官方参考) |
|---|---:|---:|---:|---:|
| Text Edit↓ | 0.0697 | **0.0367** | 0.0481 | 0.0630 |
| Formula Edit↓(非 CDM,不可直接对比官方列) | 0.1026 | **0.0948** | 0.1614 | — |
| Table TEDS↑ | **0.9061** | **0.9065** | 0.7920 | 0.8307 |
| Table TEDS-S↑ | **0.9375** | **0.9388** | 0.8259 | 0.8675 |
| Reading Order Edit↓ | 0.1357 | **0.1285** | 0.1522 | 0.1660 |

当前 V2 相对历史 surpass：Text Edit 退化 `+0.0329`、Formula Edit 退化 `+0.0077`、Table TEDS
下降 `0.0004`、TEDS-S 下降 `0.0014`、Reading Order Edit 退化 `+0.0073`。表格基本持平，但文本
和阅读顺序回退明确，因此当前 V2 的结论是**运行完整性与稳定性通过，质量未超过历史最佳基线**。

相对 Qwen3.8-27B，当前 V2 的 Formula Edit 改善 `0.0588`、Table TEDS 提升 `0.1141`、TEDS-S
提升 `0.1116`、Reading Order Edit 改善 `0.0165`，但 Text Edit 较差 `0.0216`。这说明文档专用模型
在公式、表格和顺序上优势明显，纯文本识别并非当前链路的强项。

### 1.2 Qwen3.8-27B 历史实验解读

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
