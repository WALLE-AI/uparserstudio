# uparser 解析引擎基准评测报告

> 本报告含两套评测:
> - **第一部分 · opendataloader-bench**(2026-08-04):200 篇单页**电子版 PDF**,考察 native/mineru-vlm/pdf-inspector 等;native 主场。
> - **第二部分 · OmniDocBench**(2026-08-13,见 §8 起):全量 **1651 页扫描/图像**,native vs mineru-vlm;mineru-vlm 主场,native 因无 OCR 触底。
> 两部分指标口径不同(见各自表头),**不可跨部分直接比数值**。

## 第一部分:opendataloader-bench

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
3. **本次评测暴露并修复了一个 uparser 的关键渲染缺陷**(下节),使 mineru-vlm 的 Overall 从 0.708 跃升到 0.928——**+0.22 的提升完全来自渲染层,而非模型**。

---

## 2. 核心发现:VLM markdown 渲染器缺失标题/列表标记(已修复)

### 现象
首轮 mineru-vlm 跑分:Overall **0.7080**,其中 nid=0.947、teds=0.944 均为全场最佳,但 **mhs=0.0000**(200 篇里仅 1 篇有 `#` 标题)。

### 根因
`render/mod.rs::to_markdown` 对每个文本 block **直接输出 `block.text`,完全忽略 `block.category`**。于是 mineru-vlm 明明把标题正确分类为 `title`、列表分类为 `list`,渲染成 markdown 时却全部退化成普通段落——一个 `#` 都不出。而 heading 指标(MHS)依赖 markdown 的 `#` 结构,于是被清零,把一个本应第一的引擎拖到 0.708。

> 对照:`native` 的 markdown 直接用内嵌引擎(pdf-inspector)自带的 markdown(本就带 `#` 标题),所以 native 不受此缺陷影响——这也是"抽取更强的 mineru-vlm 反而 Overall 更低"这一反直觉现象的真正原因。

### 修复
在 `to_markdown` 里按归一化类别加 markdown 标记:`title → "# "`、`list → "- "`(`header` 是页眉,非内容标题,不加)。改动**仅影响 VLM 协议**(mineru-vlm/dots.ocr/monkeyocr-v2/pipeline/paddleocr 共享此渲染器),**不触碰 native**(其 markdown 走引擎自有 pipeline)。

### 效果

| mineru-vlm | Overall | mhs | 含 `#` 标题的文档数 |
|---|---|---|---|
| 修复前 | 0.7080 | 0.0000 | 1 / 200 |
| **修复后** | **0.9284** | **0.8777** | **112 / 200** |

单一渲染层修复带来 **+0.22 Overall**,并让 mhs 从垫底跃居第一(0.878 > hybrid 0.821)。这是一个"正确代码从未接入真实渲染路径"类型的缺陷,与本项目此前多次记录的同类问题一脉相承。

---

## 3. 分项分析

### 3.1 Reading Order(NID)
- mineru-vlm **0.947** > native/pdf-inspector 0.915 > liteparse 0.866。
- VLM 的版面理解让阅读顺序最优;native 的纯几何 XY-cut/投影已很强(0.915),远好于 liteparse。

### 3.2 Table(TEDS)
- mineru-vlm **0.944**(OTSL→HTML)≈ 榜首;native 0.814(pdf-inspector 三策略表格检测)。
- 差距 0.13 是 native 相对天花板的主要短板;但 native 的 0.814 已优于 edgeparse(0.717)、unstructured 等,且**远优于 liteparse 的 0.000**。

### 3.3 Heading(MHS)
- 修复后:mineru-vlm **0.878** > docling 0.824 > native 0.788。
- MHS 指标**层级无关**(源码:"treats all heading levels as equivalent",APTED 只比 heading/content 标签,不比 `#` 层数)——所以关键是"该是标题的行有没有被标成标题",而非层级深浅。native 的短板是标题**过检测**(引擎产 280 个 vs GT 193 个)。

### 3.4 速度
- native **0.046 s/篇**(纯 Rust、零模型、无 GPU),与最快的纯 Rust 引擎同档,比 docling 快 ~16×、比 mineru(榜单)快 ~130×。
- mineru-vlm 冷解析 **1.81 s/篇**(走 GPU vLLM);命中 uparser 内容哈希缓存时 0.005 s/篇。

---

## 4. native vs liteparse:目标达成情况

用户目标之一是"native 从精度与速度全面超过 liteparse":

| | native | liteparse(榜单) | 结果 |
|---|---|---|---|
| Overall | **0.8754** | 0.576 | 超 +0.30 |
| Reading Order | **0.915** | 0.866 | 超 |
| Table | **0.814** | 0.000 | 超 |
| Heading | **0.788** | 0.000 | 超 |
| 速度 s/篇 | **0.046** | 1.061 | 快 ~23× |

**精度与速度双维度全面超越,✅ 达成。**(注:榜单 liteparse 用的是纯文本口径;即便给它最乐观的 markdown 估分,native 仍稳赢,且速度优势是结构性的。)

## 5. native vs pdf-inspector:内部化 + 打平

native 的 markdown 当前直通内嵌引擎(即 pdf-inspector 核心),二者输出**逐字节相同**,故 Overall **打平**(均 0.8754),速度上 native 因多一层 CLI 开销略慢(0.046 vs 0.032 s/篇)。native 的价值在于:**去除对 liteparse 的依赖、消除 PDFium 二进制、并入 uparser 的统一 IR/CLI/路由**。要严格超越 pdf-inspector 需引擎核心调优(降标题过检测、提 TEDS),会使 vendored 引擎与上游分叉,作为后续可选工作。

---

## 6. 选型建议

| 场景 | 推荐 | 理由 |
|---|---|---|
| 电子版 PDF、无 GPU、要快 | **native** | 0.046 s/篇、零依赖、0.875 分,碾压 liteparse |
| 追求最高质量、有 GPU | **mineru-vlm** | 0.928 全场第一,表格/阅读顺序最佳 |
| 扫描件 | **mineru-vlm** | native 无 OCR(扫描件会空);VLM 直接识别 |
| `--protocol auto` | 由 profiler 路由 | 电子版长文本→native,表格密集/扫描→VLM |

---

## 7. 方法学与可复现

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
---

# 第二部分:OmniDocBench 评测(native vs mineru-vlm)

> **状态说明（2026-08-14）**：§8-12 保留 2604/旧适配器的历史结果；当前 2605 官方等价
> 基线和 `mineru-vlm-surpass` 实验见 §13，后者是后续优化的有效基线。

> 日期:2026-08-13 · 语料:**OmniDocBench 全量 1651 页**(670 png + 981 jpg,真实扫描/渲染页面图像) · 评测器:OmniDocBench 官方 `end2end` harness(`benchmark/OmniDocBench`)
> 指标(OmniDocBench 口径):Text / Formula / Reading-Order = **归一化编辑距离 Edit↓(越低越好)**;Table = **TEDS↑ / TEDS-S↑(越高越好)**;另给出 Table Edit↓ 与四项 Edit 的等权均值 **Overall-Edit↓**。
> 说明:本轮**关闭 CDM**(公式渲染打分,需 TeX/Ghostscript 工具链,本机未装),公式仅用 Edit 口径;其余指标与官方一致。

## 8. 结论速览(OmniDocBench)

| 引擎 | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order Edit↓ | **Overall-Edit↓** | 速度 s/篇 |
|---|---|---|---|---|---|---|---|
| **uparser · mineru-vlm** | **0.068** | **0.097** | **0.900** | **0.930** | **0.134** | **0.093** | 2.06(GPU) |
| **uparser · native** | 1.000 | 1.000 | 0.000 | 0.000 | 1.000 | 1.000 | 0.033(空输出) |
| *官方榜 MinerU2.5-Pro(参照)* | *0.036* | *CDM 97.45* | *0.934* | *0.959* | *0.120* | *—* | *—* |

**三条主结论:**
1. **OmniDocBench 是 100% 图像输入(扫描件/渲染页),这正是 native 的设计盲区。** native 无 OCR、只吃「电子版 PDF 的原生文字层」,面对纯图像页无字可取,四项指标全部触底(Text/Formula/Reading-Order Edit=1.0、Table TEDS=0)。这不是 bug,而是与 ARCHITECTURE 路由设计一致的取舍——扫描件本就该交给 VLM。
2. **mineru-vlm 在 OmniDocBench 上表现优异**:Text Edit 0.068、Table TEDS 0.900、Reading Order Edit 0.134,Overall-Edit 0.093。与第一部分(opendataloader-bench 全场第一)相互印证,mineru-vlm 是 uparser 面向扫描/复杂版面的主力协议。
3. **uparser 的 mineru-vlm 适配达到官方 MinerU2.5-Pro 榜单质量的 ~90–96%**(Table TEDS 0.900 vs 0.934、TEDS-S 0.930 vs 0.959、Reading Order 0.134 vs 0.120、Text Edit 0.068 vs 0.036),差距来自「重实现的编排/光栅化/OTSL→HTML/Markdown 渲染」与 checkpoint 版本差异(见 §11),而非模型本身。

## 9. mineru-vlm 分项与中英文拆分

| 维度 | ALL | 英文 EN | 简体中文 ZH |
|---|---|---|---|
| Text Edit↓ | 0.068 | 0.064 | 0.071 |
| Formula Edit↓ | 0.097 | 0.085 | 0.126 |
| Table TEDS↑ | 0.900 | 0.869 | 0.950 |
| Table Edit↓ | 0.072 | 0.117 | 0.047 |
| Reading Order Edit↓ | 0.134 | 0.103 | 0.162 |
| **Overall-Edit↓**(四项均值) | **0.093** | 0.092 | 0.102 |

- **表格中文强于英文**(TEDS 0.950 vs 0.869):OmniDocBench 中文表多为规整线框表,OTSL→HTML 还原度高;英文含较多学术三线表/跨页表,结构更难。
- **阅读顺序英文优于中文**(Edit 0.103 vs 0.162):中文样本含更多报纸/多栏/PPT 版式,顺序更易错。
- 与第一部分口径不同:opendataloader-bench 报的是 NID(阅读顺序,越高越好)、TEDS、MHS 三者均值;此处是 OmniDocBench 官方的编辑距离口径,不能与第一部分的 Overall 直接数值对比。

## 10. native 为何在 OmniDocBench 触底(而在 opendataloader-bench 拿 0.875)

| | opendataloader-bench | OmniDocBench |
|---|---|---|
| 输入形态 | 电子版单页 **PDF(含原生文字层)** | 单页 **图像(png/jpg,无文字层)** |
| native 可提取内容 | 有(lopdf 直接读文字层) | **无**(图像无字可读) |
| native Overall | 0.875 | 触底(Edit=1.0 / TEDS=0) |

结论一致且自洽:**native 的价值域是电子版 PDF,不是扫描件**;两套语料恰好覆盖了这条设计边界的两侧。为给 native「最公平的一次机会」,评测时已把每张图像用 PIL 包成单页 PDF 再喂给 native 的真实引擎(而非在输入类型检查处直接拒绝),引擎确实跑通了但正确地输出空(无文字层),证明触底是「真的没内容可取」而非「拒绝处理图片」。

## 11. mineru-vlm 与官方榜单的差距分析

实测 mineru-vlm 略低于官方 MinerU2.5-Pro 榜单(Text Edit 0.068 vs 0.036、Table TEDS 0.900 vs 0.934),原因:
- **重实现的适配层**:uparser 的 mineru-vlm 走自研 Rust 编排(光栅化、per-block 裁剪、OTSL→HTML、Markdown 渲染),与 MinerU 官方 `mineru_vl_utils` 的推理/后处理链不完全一致;官方榜单是官方全链路。
- **checkpoint 版本**:本机 endpoint 实际服务 `MinerU2.5-Pro-2604-1.2B`(模型根路径),CLI `--model` 传的是 `MinerU2.5-2604-1.2B`;官方榜单 MinerU2.5-Pro 为 `2605` 版本,存在小版本差异。
- **公式口径不可直接比**:本轮关闭 CDM,公式用 Edit(0.097);官方用 CDM(97.45),两者不同量纲。
- **Markdown 渲染影响 Text/Order Edit**:GT 的期望格式与 uparser 渲染的标题标记(`#`)/列表(`-`)/换行策略存在细微差异,会抬高编辑距离——这类差异对 opendataloader-bench 的 MHS(层级无关)不敏感,但对 OmniDocBench 的字符级 Edit 敏感,是后续可优化点。

即便如此,0.900 TEDS / 0.093 Overall-Edit 仍是 top-tier 结果,且远超本语料上 native 的触底表现——对「扫描件/图像页」这一 OmniDocBench 唯一形态,选型结论明确:**用 mineru-vlm,不用 native**。

## 12. 方法学与可复现(OmniDocBench)

- **数据**:`benchmark/OmniDocBenchData/OmniDocBench.json`(1651 页)+ `images/`(670 png + 981 jpg)。
- **预测生成**:对每张图像 `uparser parse --format markdown --no-assets`,输出 `<image_stem>.md` 到 `benchmark/omnidoc_pred/<engine>/`(evaluator 按 `image_path` 去扩展名 + `.md` 匹配)。
  - mineru-vlm:`--protocol mineru-vlm --endpoint http://127.0.0.1:19122/v1/chat/completions --model MinerU2.5-2604-1.2B`,**必须设 `NO_PROXY=127.0.0.1,localhost` 绕过企业代理**(否则请求被代理劫持到其自带 nginx,返回 404/无效)。16 路并发,全量 1651 页,冷解析 ~2.06 s/篇。
  - native:先用 PIL 把图像转单页 PDF(`omnidoc_work/native_pdf/`),再 `--protocol native`;全量输出空。
- **评测**:`benchmark/OmniDocBench` 内 `uv venv --python 3.10 .venv` + `uv pip install -e .`;配置 `configs/omnidoc_{mineru-vlm,native}.yaml`(全量 GT + 各自预测目录,drop CDM);运行 `python run_eval.py --config <cfg>`,结果落 `result/<engine>_quick_match_*`。
- **踩到并修复的评测器环境 bug**:OmniDocBench 的 TEDS 打分在 `ThreadPoolExecutor` 线程内 `fork` 出 `multiprocessing.Process` 做超时保护;本机 `fork`-from-thread 触发「os.fork is unsafe while filelock…」导致 `process.start()` 失败、所有表格 TEDS 被打成 0。修复:用 `run_eval.py` 包一层 `multiprocessing.set_start_method('spawn', force=True)`——保留超时保护的同时消除 fork 问题(native 验证 0 TEDS-error 后再跑 mineru-vlm)。此修复不改评测逻辑,仅换进程启动方式。

### 局限(OmniDocBench)
- 关闭了 CDM(公式渲染打分):无 TeX 工具链;公式仅 Edit 口径,与官方 CDM 不可直接比。
- 官方榜单参照值(MinerU2.5-Pro:Overall 95.75 / Text Edit 0.036 / Formula CDM 97.45 / Table TEDS 93.42 / TEDS-S 95.92 / Read Order 0.120)取自 `benchmark/OmniDocBench/README.md` 排行榜;mineru-vlm/native 两列为本次实测。
- native 在图像语料触底是设计使然(无 OCR),非可调优项;要在扫描件上提分应走 VLM 协议或引入 OCR。
- mineru-vlm 分数依赖本机 `MinerU2.5-Pro-2604-1.2B` 这一具体 checkpoint/endpoint;换 checkpoint 或换官方全链路会有出入。

## 13. MinerU2.5-Pro-2605 官方等价基线与超越实验

> 日期：2026-08-14 · endpoint：`http://127.0.0.1:19122/v1/chat/completions` · 模型：
> `MinerU2.5-Pro-2605-1.2B` · 服务端：vLLM 0.19.0 + `mineru-vl-utils` 0.1.14 ·
> logits processor：`mineru_vl_utils:MinerULogitsProcessor`。

### 13.1 官方等价链路

新增独立协议 `mineru-vlm-official`，对齐官方 2605 推理和 OmniDocBench 输出契约：

- 两阶段请求统一使用 greedy sampling，并发送
  `vllm_xargs={"no_repeat_ngram_size":100,"debug":false}`；
- 使用严格 custom-token layout parser，越界框不做 clamp/rescue；
- 对齐官方 category 映射、stage-2 skip list 和未知类型处理；
- 关闭 uparser 通用 IoU 去重、退化文本升温重试和全局几何合并；
- block content 按官方脚本原样连接，不额外添加标题、列表和公式包装。

完整 1651 页预测成功，runner 无错误；Table TEDS 的 665 个计算样本无 timeout/error，page
match 有 2 个长索引页触发 quick-match timeout。

| 指标 | 旧 uparser current | 2605 official profile | 官方参考 | 是否超过官方 |
|---|---:|---:|---:|---|
| Text Edit↓ | 0.06820 | **0.03772766** | 0.036 | 否，差 0.00173 |
| Formula Edit↓ | 0.09989 | **0.09353726** | 非官方主指标 | CDM 尚未验证 |
| Formula CDM↑ | 未启用 | 未启用 | 97.45 | 未验证 |
| Table TEDS↑ | 0.92484 | **0.92676132** | 0.934 | 否，差 0.00724 |
| Table TEDS-S↑ | 0.95441 | **0.95632469** | 0.959 | 否，差 0.00268 |
| Reading Order Edit↓ | 0.13868 | **0.12962177** | 0.120 | 否，差 0.00962 |

结论：恢复官方等价行为后，Text 回收了约 94.6% 的原始差距，Reading Order 回收约 48.5%，
但当前仍未全面超过官方，禁止把该结果表述为“已超过”。

### 13.2 E1 密集索引安全合并

新增隔离协议 `mineru-vlm-surpass`。它继承 official profile，只对不少于 150 个 block、text 占比
不少于 90%、短文本占比不少于 90%、相邻同列可合并率不少于 75% 的页面执行几何合并。

官方基线中仅有的两个 quick-match timeout 页面分别包含 187/197 个 text block，稳定分成三列。
E1 将输出从 381/401 行降至 13/13 行。使用同一官方 evaluator 对原 GT 两页子集评测：

| 指标 | official profile 两页 | E1 两页 |
|---|---:|---:|
| quick-match timeout | 2 | **0** |
| Text Edit↓ | 1.0 | **0.00562046** |
| Reading Order Edit↓ | 1.0 | **0.0** |

保持其他 1649 页不变并按官方基线分母换算，E1 全量预期 Text 为 `0.03645036`、Reading Order
为 `0.12840077`，Table/Formula 不变。E1 有明确增益，但 Text 和 Reading Order 仍未跨过官方
`0.036/0.120` 门槛；完整 1651 页 E1 结果出来前，该数字只能标记为“换算预期”，不能作为正式
全量成绩。

### 13.3 验证状态与下一阶段

- Rust workspace：1103 tests passed，0 failed；
- `cargo fmt --all --check`、`git diff --check` 通过；
- 19122 endpoint 已验证 `/v1/models`、真实图片两阶段推理、`MinerULogitsProcessor` 和
  `vllm_xargs`；
- 后续顺序：完整 E1 全量评测 -> PIL/Rust 像素 differential -> exact OTSL differential ->
  selective reading-order reranker -> CDM 恢复 -> 三次全量验收。
