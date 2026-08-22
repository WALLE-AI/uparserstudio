# UParser V2 质量与性能全面领先优化执行计划

> 状态：执行中（W0/W0-C 已形成首轮正式基线，G-S/G-C 尚未全通过）
> 制定日期：2026-08-21  
> 最近执行更新：2026-08-22
> 对应架构：`ARCHITECTURE_V2.0_PROPOSAL.md` §1.1 G-S、§12 V11  
> 目标：在保留 `uparser-native-engine`、`uparser-document-engine` 和 `uparser-core` 既有文档算法的前提下，使 V2 不仅超过旧方案，还要在冻结版本、共同能力范围和同等资源配置下，对 LiteParse、Anydoc 及同类开源框架形成可复现的质量与性能严格领先。
>
> 声明边界：本文的“全面领先”不是无版本、无语料、无资源上限的绝对断言，而是通过本文 G-C 闸门后，对**已冻结竞品版本 × 已公布数据集 × 已定义资源等级 × 共同支持格式**的可审计结论。任一单元格未通过，就只能报告逐项结果，不能宣称“全面超过”。

### 当前执行快照（2026-08-22）

- W0-C 已落地竞品锁、七轮随机轮换 CLI harness、逐样本 paired bootstrap、轮间 CV、Windows RSS 采集、机器闸门和竞品报告。
- 当前 release `uparser.exe --mode native` 已完成 PDF-Public 200 篇七轮交错复测。其 median/P95/吞吐为 `27.117 ms / 33.965 ms / 35.8239 docs/s`，优于 pdf-inspector 的 `34.864 ms / 72.439 ms / 24.4583 docs/s` 和 LiteParse 的 `43.767 ms / 57.549 ms / 21.8758 docs/s`；但相对 pdf-inspector 的峰值 RSS 更高（`17,854,464` 对 `15,458,304` bytes），所以该对手的 G-C2 仍为 `FAIL`。LiteParse 的性能门禁还受其轮间 CV 阻断。
- PDF-Public 质量相对 pdf-inspector 的 Overall 平均提升 `0.017568`，95% CI 为 `[0.010333, 0.026059]`；NID、TEDS、MHS 也全部显著领先，TEDS CI 为 `[0.008479, 0.068250]`，因此该对手的 G-C1 保持 `PASS`。
- PDF-Public 质量相对 LiteParse 的 Overall 平均提升 `0.009460`，95% CI 为 `[-0.000486, 0.019836]`；四项主指标均值全部领先，TEDS 已显著领先，但 Overall、NID、MHS 的 CI 仍跨 0，且 Overall 未达到 1% 门槛。
- W1 三个独立包 LLVM 行覆盖率组合达到 `90.64%`（`60954/67245`）；`uparser-core` 为 `92.01%`，`uparser-document-engine` 为 `88.03%`，`uparser-native-engine` 为 `90.84%`，包级/组合门槛全部通过。Native `lib.rs` 为 `88.01%`；当前测试清单 1420 项，其中 863 个 Native 测试零失败。W1 的变更行覆盖率 95% 仍需独立产物确认。
- W2 的当前统一执行器质量为 Overall/NID/TEDS/MHS `0.893120/0.919322/0.848157/0.842314`，相对冻结 heading-guard 基线四项分别提升约 `0.007353/0.003765/0.033018/0.009065`。TOC、日期续行、表题、宽表头和数值表头修复累计只改变 200 个输出中的 8 个，其余 192 个哈希一致。当前 SHA 的独立五轮性能为 median `26.466 ms`、P95 `35.971 ms`、峰值 RSS `18,595,840` bytes、轮间 median CV `2.439%`，1000/1000 成功且无输出哈希漂移；稳定性线已通过，但旧方案没有冻结 P95，且当前显式 Native in-process fast path 仍需对照 6.2 的“无特殊旁路”约束完成架构审查，因此 W2 尚未完全关闭。
- 最终统一 Office 七轮中 UParser median/P95 为 `15.695/18.691 ms`，慢于 Anydoc 的 `13.996/16.662 ms`，吞吐更低且峰值 RSS 更高；Office 语义 evaluator 仍缺失。Office-Holdout、PDF-Generalization、Robustness、G-R、变更行覆盖率及外部服务闸门同样待完成。因此当前 `gate_GC.json` 为 `FAIL`，不得宣称“全面领先”。

## 1. 当前基线与差距

| 维度 | 当前结果 | 发布要求 | 差距 |
|---|---:|---:|---|
| 三个核心包行覆盖率 | 90.64%（60954/67245） | ≥90% | 已通过 |
| `uparser-core` 行覆盖率 | 92.01%（10540/11455） | ≥92% | 已通过 |
| `uparser-document-engine` 行覆盖率 | 88.03%（8275/9400） | ≥88% | 已通过 |
| `uparser-native-engine` 行覆盖率 | 90.84%（42139/46390） | ≥90% | 已通过 |
| Native Bench A Overall | 0.893120 | 不低于冻结 0.885767 且产生严格改进 | 提升约 0.007353；四项均不退 |
| Native 性能 | 五轮 median 0.026466 s/doc；七轮竞品交错 median 0.027117 s/doc | ≤0.047341 s/doc，且旧 P95 不退 | median/CV 已过；旧 P95/完整 W2 未关闭 |
| MinerU Bench A Overall | 0.923978 | ≥0.928368，且同 checkpoint A/B 不退 | 回退 0.004390，当前比较还存在 checkpoint 混淆 |
| Auto 相对 Native Overall | +0.016598 | ≥+0.02 | 差 0.003402，且缺人工 G-R |
| OmniDocBench | 文本/公式/顺序及两项表格均弱于指定历史基线 | 五项不退，至少一项改善，空白输出为 0 | 未通过 |
| LiteParse | 当前 `crates-v2.12.0`；统一执行器 median/P95/吞吐和四项质量均值更优 | 当前 checkout 同机、同语料、同资源下质量和性能逐项领先 | **未通过**：仅 TEDS CI 严格领先，Overall 提升不足 1%；竞品性能 round CV 未过 |
| pdf-inspector | 当前 `v1.14.2-2-g2543abe`；统一执行器 median/P95/吞吐更优，质量 G-C1 已通过 | 当前 checkout 下 Native 的主质量指标与性能矩阵全部严格领先 | **未通过**：质量已通过；RSS 更高，性能 G-C2 失败 |
| Anydoc | 当前 `v0.1.9`；统一执行器 Office median/P95/RSS/吞吐均落后 | 共同格式质量、健壮性和各格式性能矩阵全部领先 | **未通过**：性能失败，语义质量证据不足，legacy DOC 样式仍是缺口 |

当前工作的重点不是继续抽象 runner，而是先把历史“榜单引用”和“小样本观察”升级为当前竞品版本的同机成对实验，再用逐样本差值、失败簇、覆盖率和 profile 驱动优化。

### 1.1 竞品范围与版本策略

| 优先级 | 对照对象 | 强制比较路径 | 冻结策略 |
|---|---|---|---|
| P0 | `opensource/liteparse` | PDF 文本层、复杂 PDF、扫描/混合 PDF；CLI 与进程内库接口分别测 | 以仓库内 checkout 为执行基线；当前为 `crates-v2.12.0` / `2fd644a9...` |
| P0 | `opensource/anydoc` | DOC(X)、PPT(X)、XLS(X)、ODF、RTF、EPUB、CSV/TSV 及 PDF 共同能力 | 以仓库内 checkout 为执行基线；当前为 `v0.1.9` / `e754e1d3...` |
| P0 | `opensource/pdf-inspector` | 无模型 PDF 原生解析、逐文档 Markdown 和进程内吞吐 | 以仓库内 checkout 为执行基线；当前为 `v1.14.2-2-g2543abe` / `2543abe3...`；与 Native 的算法血缘必须披露 |
| P1 | Docling、MarkItDown | 各自共同支持格式和默认本地模式 | 固定最新稳定版；无法同资源运行的后端单独分层，不混入结论 |
| P1 | MinerU 等开源模型框架 | GPU 文档理解与 OmniDocBench | 固定模型权重、推理引擎和显存配置；模型能力与 runner 开销分别报告 |

- 开始 W0 时生成 `competitor_lock.json`，记录 tag、commit、二进制哈希、许可证、构建参数和默认配置。
- 发布候选前 14 天执行一次 upstream refresh；若有新稳定版，重跑全部强制矩阵。截止日之后发布的版本列入“发布后观察”，不阻塞本次结论。
- 竞品本身不支持的格式标记 `N/A`，不得记为失败；UParser 声称支持而失败的样本必须记失败，不得从分母剔除。
- 竞品 README 数字和历史榜单只用于候选筛选，不能替代同机复跑。

### 1.2 “全部超过”的可验收定义（G-C）

G-C（Competitor Dominance Gate）是 G-S 之外的外部发布闸门。只有以下四项全部通过，才允许使用“在冻结竞品矩阵中质量与性能全面领先”：

1. **G-C1 质量严格领先**：每个强制数据集的每个主指标，UParser 的成对差值方向都更优，95% bootstrap 置信区间不得跨 0；同时归一化综合分至少领先 `1%`。不得以表格提升抵消文本、顺序或标题回退。
2. **G-C2 性能严格领先**：在同一资源等级内，median、P95、吞吐、峰值 RSS 四项均优于对应竞品；耗时比的 95% 置信区间上界 `<1.0`，吞吐比下界 `>1.0`。发布目标额外要求 median 至少快 `10%`，否则只能称“统计领先”，不能称“显著更快”。
3. **G-C3 可靠性严格领先**：成功率不低于竞品且 UParser 必须为 `100%`；超时、崩溃、空白输出、资源预算越界均为 0；畸形输入的正确拒绝/有界恢复 F1 必须严格更高。共同为满分时，可靠性只判定持平，必须由质量与性能形成严格领先。
4. **G-C4 无分桶失败**：PDF 按语言、扫描质量、版式、表格/公式难度分桶；多格式按格式、文件大小和正常/破损分桶。任何样本数 `n>=30` 的强制桶只要质量或性能落后，即整体闸门失败，不允许用 macro/micro 平均掩盖。

若某指标已共同达到理论上限，允许该指标“持平”而不要求伪造严格提升，但同一比较单元必须至少有一个质量指标和一个性能指标严格领先。

## 2. 执行原则

1. **先可信测量，再改代码**：没有同配置成对 A/B，不接受“看起来更快”或“模型分数更高”的结论。
2. **架构优化与算法优化分开提交**：前者必须输出保真；后者必须附逐样本差值和消融结果。
3. **质量护栏优先**：任何 NID/TEDS/MHS、Text/Formula/RO Edit 的显著回退都阻断合并。
4. **不旁路既有算法**：不得通过删除 Native、结构化源语义或 engine Markdown 链换取性能数字。
5. **只优化已定位热点**：先用分阶段 timing、CPU profile、分配统计定位 3.47 ms，再决定实现。
6. **逐候选晋级**：单元/fixture → Bench A 20 文档 smoke → Bench A 200 文档 → OmniDocBench 分层集 → 1651 页全量。
7. **一次只验证一个假设**：每个候选变更有独立开关或提交，可单独回滚。

## 3. 总体执行顺序

```text
W0 冻结可比基线
  ↓
W0-C 建立竞品评测矩阵与独立冻结集
  ↓
W1 补齐高风险覆盖率
  ↓
W2 消除 Native runner 架构税
  ↓
W3 提升 Native 与结构化格式精度
  ↓
W4 恢复并提升 MinerU / OmniDocBench 精度
  ↓
W5 提升 Auto 路由收益
  ↓
W6 CI 固化与 G-S/G-C 全量验收
```

新增竞品矩阵作为 `W0-C` 插入 W0，并在所有优化前完成。任何性能或精度改动必须在 W0/W0-C 完成后开始。

## 4. W0：冻结可信基线与评测工具

### 4.1 任务

| 编号 | 任务 | 代码/产物落点 |
|---|---|---|
| W0-1 | 固定旧 runner 与 V2 runner 的二进制、构建特性和 SHA-256 | `benchmark/baselines/architecture_v2_surpass.json` |
| W0-2 | 固定 MinerU checkpoint、endpoint、采样参数、DPI、prompt、worker 和 vLLM 服务参数 | 同一 baseline manifest |
| W0-3 | 扩展评测器支持旧/新 runner 随机交错五轮 A/B、禁用缓存、预热和逐样本耗时 | `benchmark/evaluate_architecture_v2.py` |
| W0-4 | 在 runner 中增加 detect/analyze/route/materialize/execute/postprocess/assets 的阶段计时 | `uparser-core/src/runner.rs`、现有 `timing` 元数据 |
| W0-5 | 增加 artifact 计数器测试，证明 PDF/结构化文档每次只解析一次 | `uparser-core/src/runner.rs` tests |
| W0-6 | 生成逐样本差值、median/P95、bootstrap 置信区间和峰值 RSS | `benchmark/results/` 下带日期 JSON 与 Markdown 摘要 |

### 4.2 退出条件

- 旧/新 runner 在同一 MinerU 服务上的输入图片哈希、prompt 哈希和请求参数完全一致。
- Native 五轮基准的轮间中位数波动不超过 2%；超过时先控制 CPU 调频、后台负载和文件缓存。
- 报告能区分框架耗时、PDF 解析耗时、模型服务耗时和评测器耗时。
- 禁止继续使用历史命中缓存的 `1.81 s/doc` 作为性能基线。

### 4.3 W0-C：竞品矩阵与评测资产

#### 4.3.1 公平配置

所有引擎同时提供两种口径，禁止只选择对 UParser 有利的一种：

| 口径 | 用途 | 约束 |
|---|---|---|
| `library-hot` | 比较解析算法本身 | 同一长驻进程、单线程、相同输入 bytes、预热一次、禁用结果缓存；输出必须物化到内存 |
| `cli-e2e` | 比较真实用户体验 | 每次独立进程，包含启动、读取、解析、渲染和写出；相同输出格式与资产策略 |

资源等级必须分开报告，不得跨等级得出快慢结论：

| 等级 | CPU/GPU | OCR/模型 | 强制对照 |
|---|---|---|---|
| R0 原生文本 | 固定 CPU 核数和内存；无 GPU | OCR、LibreOffice 转换和模型全部关闭 | UParser Native vs LiteParse text-only vs pdf-inspector；结构化格式对 Anydoc |
| R1 本地 OCR | 同 CPU；允许固定 OCR 模型 | 模型、语言包、线程数完全相同 | UParser Auto/可用 OCR 路径 vs LiteParse OCR；只在双方能力可达后启用 |
| R2 GPU 模型 | 同 GPU、显存上限、batch/worker | 固定同一 checkpoint 或分别报告产品默认 | UParser model protocol vs MinerU 等模型框架 |

若竞品依赖 LibreOffice 才支持某格式，该结果归入“外部转换器”分层，不能与纯 Rust R0 的算法耗时混报。PDF 资产提取、页范围、密码、OCR、Markdown/JSON 输出选项必须通过 capability manifest 显式对齐。

#### 4.3.2 数据集与防过拟合

| 套件 | 规模/来源 | 目的 | 强制指标 |
|---|---|---|---|
| PDF-Public | OpenDataLoader 200 篇 | 与既有报告连续，重跑当前 LiteParse/pdf-inspector | Overall、NID、TEDS、MHS、成功率、median/P95/RSS |
| PDF-Generalization | 冻结前未参与调参的独立 PDF，至少 500 篇 | 覆盖中英、RTL、多栏、表格、公式、表单、扫描/混合、超长文档 | CER/WER、内容 F1、RO、TEDS、MHS、资产召回、逐桶指标 |
| Office-Public | `opensource/anydoc/tests/fixtures` 仅作公开兼容集 | 复现当前 47/47 与对抗样本 | 成功/拒绝/恢复、字段级语义 diff |
| Office-Holdout | clean-room 或许可清晰的独立冻结集，每个强制格式 `n>=30`，总计至少 450 篇 | 防止针对 Anydoc fixture 过拟合 | 文本、标题、列表、样式、表格、链接、图片、notes、公式/类型保真 |
| Robustness | 每格式至少 20 个 malformed/mutation seed，累计至少 300 个 | 安全与有界恢复 | 判定 F1、超时、panic、峰值内存、warning/error code |

- Public 集允许定位问题；Holdout 的 GT 在功能冻结前对开发者隐藏，只允许 CI 返回分项结果和失败样本 ID，不返回答案内容。
- 真实文档与程序生成文档至少各占 40%；任何单一生产工具不得超过 25%。
- PDF 短文档与长文档分别统计；Office 按 `small <1 MiB`、`medium 1–10 MiB`、`large >10 MiB` 分桶。
- 当前 47 个 Anydoc fixture 只能证明兼容性，不能单独支撑“质量全面领先”。
- 性能只报告实测分布：禁止使用 best-of-N、手工扣除估算启动时间、缓存命中或输出更少的配置。启动成本通过 `library-hot`/`cli-e2e` 两种口径自然分离。

#### 4.3.3 评测器建设

新增统一入口 `benchmark/compare_competitors.py`，只负责调度和收集；格式评测使用结构化解析器，不用 Markdown 字符串启发式：

```text
competitor_lock.json
  -> capability preflight
  -> randomized Latin-square execution (7 measured rounds)
  -> canonical output adapters
  -> paired quality/performance scoring
  -> per-sample deltas + bootstrap CI
  -> gate_GC.json + Markdown report
```

必须产出：原始 stdout/stderr、返回码、逐阶段时间、CPU 时间、峰值 RSS、输出哈希、配置哈希、逐样本质量分、分桶结果和 10,000 次 paired bootstrap 结果。评测器自身要有 golden、方向性和缺失输出测试，任何超时/空输出按最差质量计分。

#### 4.3.4 W0-C 退出条件

- 三个 P0 本地对照均可从锁定 commit 可复现构建，且实际运行的二进制哈希进入 manifest。
- LiteParse 和 pdf-inspector 在 PDF-Public 上完成当前版本同机复跑；不得继续使用旧 `0.576/1.061 s/doc` 作为发布判定。
- Anydoc 47 样本、独立 Office-Holdout 和 Robustness 首轮基线完成，明确列出 UParser 的逐格式失分点。
- 七轮随机轮换的空载基线稳定，性能轮间变异系数 `CV <=3%`；否则先修复环境，不能进入性能优化。
- `gate_GC.json` 能逐单元输出 `PASS/FAIL/INSUFFICIENT`；`INSUFFICIENT` 与 `FAIL` 一样阻止“全面领先”声明。

## 5. W1：覆盖率驱动的风险补测

### 5.1 P0 模块与测试矩阵

| 模块 | 当前 | 目标 | 必须新增的场景 |
|---|---:|---:|---|
| `extractor/xobjects.rs` | 89.28% | ≥80% | **覆盖率门槛已过**；仍补不同 Filter 和真实复杂 PDF fixture |
| `tounicode.rs` | 86.08% | ≥85% | **覆盖率门槛已过**；仍补真实嵌入字体 PDF fixture 与更多损坏 CMap 容错场景 |
| `formats/sheet.rs` | 91.15% | ≥85% | **覆盖率门槛已过**；仍补 XLS、多 Sheet、隐藏 Sheet、空行、链接和资源预算行为场景 |
| `formats/doc.rs` | 96.88% | ≥80% | **覆盖率门槛已过**；仍需实现 STSH、CHPX/PAPX、SPRM、嵌入对象和加密文档语义 |
| Native `lib.rs` | 88.01% | ≥80% | **覆盖率门槛已过**；仍补真实扫描/混合 PDF、密码文档和部分页错误 fixture |

### 5.2 实施方法

- 优先从 `/home/dataset1/gaojing/llm/uparserstudio/bench/data` 和现有测试目录挑选真实样本，再补最小合成 fixture。
- 每个 fixture 同时断言内容、结构、warning/error code 和资源上限，不只断言返回成功。
- XObject 和 ToUnicode 的修复前测试先作为 characterization test 提交；算法改进在后续独立提交。
- 为解析器增加畸形输入和取消测试；纯 parser 函数适合时增加属性测试或 fuzz seed。
- 覆盖率报告按 crate 和关键文件生成，禁止排除上述 P0 文件。

### 5.3 退出条件

- 整体/core/document/native 行覆盖率分别达到 `90%/92%/88%/90%`。
- P0 文件全部达到独立门槛，变更行覆盖率达到 95%。
- 全部测试零失败，现有 Native Markdown golden 无非预期变化。

## 6. W2：消除 Native runner 架构税

### 6.1 先剖析的假设

| 假设 | 验证方式 | 允许的优化方向 |
|---|---|---|
| H-P1：CLI/runner 固定初始化占主要开销 | 分离进程启动、prepare、execute 和输出耗时 | 延迟构造非 Native registry/transport；减少不必要初始化 |
| H-P2：画像或元数据存在重复遍历/分配 | 记录 artifact 构建次数、clone 字节和阶段 timing | 画像使用 artifact 引用/摘要；避免大 Vec/String clone |
| H-P3：显式 Native 执行了 Auto 专属工作 | 对比 `--mode native` 与 `--mode auto` 阶段轨迹 | 显式模式跳过非执行必需 L3 和候选评分，但保留格式检测与可达性 |
| H-P4：输出/资产路径增加固定开销 | 使用 `--no-assets`、不同输出格式做消融 | 无资产时不创建目录；只在请求格式需要时 lower/render |
| H-P5：基准噪声造成表观 7.33% | 五轮交错 A/B 和置信区间 | 若差异不显著，修正结论；不能为噪声改生产代码 |

### 6.2 实施约束

- `analyze` 产生的 `PdfProcessResult` 必须继续由 `execute_native` 复用，禁止二次 `process_pdf_mem`。
- 不新增 Native 特殊旁路；优化应落在统一 runner 的惰性初始化、所有权移动和按需阶段上。
- 每个性能提交先跑逐文件 Markdown diff，再跑 Bench A 四项质量指标。
- 单项优化收益低于 0.5 ms/doc 且增加明显复杂度时不合入。

### 6.3 退出条件

当前执行证据（2026-08-22）：

- 最终 release 二进制为 `uparser/target/release/uparser.exe`，SHA-256 `A1926862B15E0209BEAFEADCF47E650ED71383A88051318EB5B4E832A006523E`。
- 当前 SHA 的五轮 200 篇稳定性测试 1000/1000 成功、超时/空输出/哈希漂移均为 0；median `26.466 ms`、P95 `35.971 ms`、吞吐 `36.2738 docs/s`、峰值 RSS `18,595,840 bytes`，相对历史 median `47.340881 ms` 改善 `44.10%`；轮间 median CV 为 `2.439%`，通过 3% 稳定性线。
- PDF-Public 官方 evaluator 四项均高于冻结 heading-guard 基线：Overall 约 `+0.007353`、NID `+0.003765`、TEDS `+0.033018`、MHS `+0.009065`。
- 七轮最终统一执行器在 PDF median/P95/吞吐上领先两项竞品；相对 pdf-inspector 仍因 RSS 未满足 G-C2，相对 LiteParse 则因竞品轮间 CV 阻断。独立五轮 CV 已通过，`library-hot` 对照和旧 P95 证据仍缺失。
- CLI 当前以统一公开二进制进入显式 in-process Native fast path。其输出契约、失败路径和质量已测试，但是否符合“不得新增 Native 特殊旁路”的实现约束尚未通过阶段轨迹/架构审查，不能仅凭性能结果关闭 W2。

- 五轮 median `≤0.047341 s/doc`，P95 不高于旧方案。
- 对本地 `opensource/pdf-inspector` 和 `opensource/liteparse` 的 PDF-Public 七轮同机测试通过 G-C2；其中 Native 不仅要消除 runner 税，还要使 `library-hot` 和 `cli-e2e` 的 median/P95/RSS 全部领先。若统一 runner 无法快于同源 pdf-inspector，优先把共享优化下沉到 `uparser-native-engine`，禁止伪造绕过 runner 的发布路径。
- Native 四项质量不下降，逐文件非预期差异为 0。
- 每文档 PDF/结构化解析次数为 1，峰值 RSS 不增加超过 5%。

## 7. W3：提升 Native 与结构化格式精度

### 7.1 优化顺序

1. **XObject 文本完整性**：修复嵌套 Form 的资源继承、变换矩阵、字体和阅读顺序；直接影响工程规范、表单和复杂 PDF。
2. **ToUnicode 与字体恢复**：按 CMap 来源建立置信级别，优先权威 ToUnicode，再使用 CID/GID/字体 cmap fallback；禁止低置信 fallback 覆盖高置信映射。
3. **Native 表格与标题**：只针对 Bench A 逐样本失败簇优化，重点提升 TEDS/MHS，不全局调阈值。
4. **结构化 Sheet/DOC**：保留 Sheet 名、merge、类型、公式、link、heading/table 和 warning；以源语义为准，不经 PDF 中转。
5. **Canonical renderer**：继续保持非默认；只有达到 engine 的 Overall/NID/TEDS/MHS 和 golden 后才重新评估切换。

### 7.1.1 面向 P0 竞品的必攻失败簇

| 对照 | 已知/预期差距 | 优化要求 |
|---|---|---|
| `opensource/pdf-inspector` | Native 当前四项质量已严格领先，median/P95/吞吐更优，但 RSS/CV 未过 | 保持 XObject、ToUnicode、表格/标题的通用增量；降低 RSS 并在独占机器把双方 round CV 压到 3% 内，不能只靠 runner 包装 |
| `opensource/liteparse` | 旧榜单已过，但当前 `v2.12.0` 增强了表格、方向文本和表单 | 重跑后按 RTL/LTR、AcroForm、复杂表格、扫描/混合页建立失败簇；所有主指标逐项领先后才关闭该项 |
| Anydoc | legacy DOC 字符/段落样式和标题层级仍落后；部分格式仅语义持平 | 优先实现 STSH、CHPX/PAPX、SPRM，随后补齐脚注/链接/列表续编号/资产定位；每格式独立过 G-C1/G-C3 |

优化顺序由“最大 G-C 失败单元对总发布阻塞的贡献”决定，而不是由容易提高的 aggregate 分数决定。每修复一个失败簇，必须加入至少一个公开回归 fixture 和一个同类隐藏护栏样本。

### 7.2 候选变更晋级规则

| 阶段 | 数据 | 通过条件 |
|---|---|---|
| 单元/fixture | P0 测试集 | 目标问题修复，既有用例全部通过 |
| Smoke | Bench A 固定 20 篇失败簇 + 20 篇随机护栏 | 目标分项改善，其他分项无回退 |
| 完整 Bench A | 200 篇 | NID/TEDS/MHS 均不退，Overall 严格提升 |
| 多格式 | 16 格式真实矩阵 | 适用语义字段全部通过，无新增失败 |

外部竞品候选另走以下晋级链，禁止跳过公开全量直接试隐藏集：

| 阶段 | 数据 | 通过条件 |
|---|---|---|
| Competitor smoke | 每个失败簇 10 个公开样本 + 10 个随机护栏 | 目标指标改善，成功率和资源预算不退 |
| Public full | PDF-Public + Office-Public + Robustness public | 对全部 P0 对照达到非劣，目标单元严格领先 |
| Shadow holdout | Office/PDF Holdout，仅返回 gate 和桶 | G-C1/G-C3/G-C4 全部通过；每个候选最多评测 2 次 |
| Release performance | 7 轮 Latin-square，独占机器 | G-C2 全部通过；原始轨迹、温度/频率和 RSS 完整 |

隐藏集连续两次失败后必须回到失败簇分析并补充**独立的公开训练样本**，不得继续试阈值。任何只对竞品 fixture 文件名、生产器特征或固定文档哈希生效的逻辑视为作弊并阻断发布。

### 7.3 退出条件

- Native Bench A 四项均不低于冻结值，Overall 或至少一个关键分项形成可复现严格提升。
- Native 在 PDF-Public 和 PDF-Generalization 上同时对当前 LiteParse/pdf-inspector 通过 G-C1；任何一个主指标或强制分桶落后均不通过。
- Sheet/DOC 真实 fixture 端到端验证通过，格式矩阵不再只验证“能够检测和返回文本”。
- 结构化文档对 Anydoc 的每格式质量、可靠性和性能均生成独立结论；所有共同能力格式通过 G-C1 至 G-C4，尤其 legacy DOC 样式不得继续以 warning 代替实现。
- 所有算法差异都有逐样本说明；无收益或产生护栏回退的候选全部回滚。

## 8. W4：恢复并提升 MinerU 与 OmniDocBench 精度

### 8.1 诊断顺序

1. 用同一 checkpoint 对旧/新 runner 比较页面图像 SHA-256、尺寸、DPI、颜色空间、prompt、请求 JSON 和解码结果。
2. 若输入和请求一致但输出不同，检查服务非确定性、采样参数、并发调度和模型版本；此时不修改 runner。
3. 若模型原始输出一致但最终 Markdown 不同，逐层检查 decode、类别映射、坐标、排序、OTSL/HTML 和 postprocess。
4. OmniDocBench 按 language、data_source、layout、table_hard、formula_hard、扫描质量拆分差值，定位文本和阅读顺序回退来源。
5. 优化候选优先采用确定性预/后处理；prompt 变更必须吸取既有“子集改善、全量反转”的结果，候选晋级前加入简单页面随机护栏。

### 8.2 晋级门槛

- 先跑覆盖困难类别和随机简单页面的分层集；任何护栏指标回退超过 0.002 即淘汰。
- 每类优化只允许一个全量候选进入 1651 页评测，减少反复全量试错。
- 最终相对 `mineru-vlm-2605-surpass-e1-full`：Text/Formula/RO Edit 不升，Table/Structure TEDS 不降，至少一项严格改善。
- 1651 页非零失败、空文件和仅空白输出均为 0。

### 8.3 退出条件

- Bench A 达到 G-S1，并且同 checkpoint 旧/新 runner A/B 证明框架无输出回退。
- OmniDocBench 五项达到 G-S1，逐类别报告和全量原始结果已保存。
- 同服务性能 A/B 的 runner median 开销满足 `≤2 ms/doc` 或 `≤2%` 中更严格的门槛。

## 9. W5：提升 Auto 路由收益

### 9.1 数据建设

- 建立覆盖 Native、MinerU 和可用 Pipeline 的多候选运行集，每个样本保存所有可行模式的真实得分与耗时。
- 人工标注文档类型、source quality、目录/标题、表格/公式/图表密度和不可接受错误。
- 分离训练/调参集与冻结验收集；验收集不得用于反复调阈值。
- 重点加入书籍、简历、招投标、法律文书、法规、合同、学术论文和财务报告的中英文真实样本。

### 9.2 路由优化

| 工作项 | 方法 |
|---|---|
| 可行性 | 先过滤格式/环境不可达模式，禁止静默 fallback |
| 质量收益估计 | 从 `DocumentProfile` 特征预测各模式相对 Native 的边际收益，而不是只预测文档类型 |
| 置信校准 | 报告 reliability curve、ECE 和低置信召回；低置信时选择稳健模式 |
| 速度约束 | 在质量收益接近时选择更快模式；路由阈值从真实质量/耗时前沿学习 |
| 可解释性 | 每次输出候选、拒绝原因、预估收益和最终 reason code |

### 9.3 退出条件

- 相对逐样本最佳可行模式的核心指标 regret `≤0.01`。
- 相对纯 Native Bench A Overall 提升至少 0.02。
- 端到端速度至少为全量 VLM 的 3 倍，失败率为 0。
- 人工冻结集的 macro-F1、Unknown/低置信召回和 calibration 均不低于冻结基线。

## 10. W6：CI、竞品领先与外部模式验收

### 10.1 CI 分层

| 层级 | 每次提交 | 每日/专机 | 发布前 |
|---|---|---|---|
| Rust tests | baseline/native/workspace/pdfium contract | 全 feature + mutation/fuzz seed | 全量零失败 |
| Coverage | 三核心包 + 变更行门槛 | P0 文件明细 | G-S3 全通过 |
| Quality | golden + 20 篇 Bench A smoke | Bench A 200 篇 | Bench A + OmniDocBench + G-R |
| Performance | Native microbench | 五轮 Native A/B | Native 与 Model-protocol 五轮 A/B |
| Formats | 16 格式受控 fixture | 真实格式矩阵 | 全字段保真报告 |
| Competitors | P0 smoke + 二进制/配置哈希检查 | PDF-Public、Office-Public 七轮比较 | PDF-Generalization、Office-Holdout、Robustness 全量 G-C |

### 10.2 外部模式

- `paddlex-structure` 和 Mode 3 pipeline 在专用服务可用后分别运行协议契约、Bench A、性能和失败注入。
- 未通过真实服务闸门的模式只能显式选择，不能进入 Auto 默认候选。
- 如果发布时服务仍不可用，报告必须明确为“V2 核心路径通过，外部模式未验收”，不能宣称所有模式领先。

### 10.3 最终产物

- `COVERAGE_REPORT.md`：更新后的整体、分包和关键文件覆盖率。
- `BENCHMARK_REPORT.md`：Bench A、OmniDocBench、Auto 和外部模式的最终对比。
- `COMPETITOR_BENCHMARK_REPORT.md`：LiteParse、pdf-inspector、Anydoc 及 P1 框架的版本锁、配置、公平性说明、逐桶质量/性能和 G-C 判定。
- `ARCHITECTURE_V2.0_EVALUATION_REPORT.md`：构建、环境、哈希、逐样本产物和 G-S 判定。
- `benchmark/results/`：机器可读原始 JSON、baseline manifest 和差值数据。
- `benchmark/baselines/competitor_lock.json` 与 `benchmark/results/gate_GC.json`：可机器判定的竞品锁和最终闸门。
- `ARCHITECTURE_V2.0_PROPOSAL.md`：仅在全部强制闸门通过后更新 V11 状态。

## 11. 提交与回滚纪律

每个优化提交必须包含以下内容：

```text
hypothesis: 要解决的具体失败簇或热点
scope: 修改模块和不修改模块
fixture: 新增/复用的测试样本
before: 覆盖率、质量、性能
after: 覆盖率、质量、性能
guardrail: 未退化的指标
rollback: 独立回滚方式或 feature flag
```

出现以下任一情况立即停止该优化方向并回滚：

- 任一关键质量指标出现统计显著回退。
- 性能收益只存在于缓存运行、单轮结果或不同模型/worker 配置。
- 为提高覆盖率删除错误处理、排除文件或降低断言强度。
- 引入第二条 Native 编排旁路或重复 PDF/结构化解析。
- 分层子集改善但随机简单页面护栏恶化，或全量结果方向反转。

## 12. 工作量与里程碑

| 里程碑 | 预计工作量 | 完成标志 |
|---|---:|---|
| M0：可信基线 | 2–4 人日 | 同配置五轮 A/B 和阶段 timing 可复现 |
| M0-C：竞品基线 | 5–8 人日 + 语料整理 | 三个本地 P0 对照可复现构建；统一 harness、当前分数和失败簇冻结 |
| M1：覆盖率 P0 | 8–12 人日 | G-S3 达标，真实/畸形 fixture 完整 |
| M2：Native/PDF 性能 | 5–10 人日 | 消除 7.33% 架构税，并对 LiteParse/pdf-inspector 通过 PDF 性能 G-C2 |
| M3：Native/结构化精度 | 12–24 人日 | Bench A 严格改善；PDF 质量双对照通过；Anydoc 共同格式逐项通过 |
| M4：MinerU/OmniDocBench | 5–10 人日 + GPU 评测时间 | 五项不退且至少一项改善 |
| M5：Auto/G-R | 6–12 人日 + 标注时间 | regret、质量增益和速度均达标 |
| M6：发布验收 | 4–7 人日 + 全量运行时间 | G-S/G-C 全绿、upstream refresh、报告与哈希固化 |

工程实现与评测约 `45–83` 人日，另需独立冻结集构建/标注投入；实际取决于当前 LiteParse/pdf-inspector 重测差距、
legacy DOC 样式、ToUnicode/XObject 失败簇和模型服务稳定性。M0-C 后应按真实差距重新估算，不能把原 `31–58`
人日继续当成承诺。竞品基线、覆盖率、Native 性能和同配置 MinerU A/B 是关键路径；在它们未通过前，不启动
大规模 prompt 或路由调参。

## 13. 完成定义

只有同时满足以下条件，执行计划才可标记完成：

- G-S1：Native、MinerU、OmniDocBench、Auto 的质量门槛全部通过。
- G-S2：Native 架构税消除，Model-protocol 同服务开销达标。
- G-S3：整体、分包、P0 文件和变更行覆盖率全部达标。
- G-S4：16 格式真实端到端语义矩阵和文档类型 G-R 冻结集通过。
- G-C1：当前锁定 LiteParse、pdf-inspector、Anydoc 的全部强制质量指标和分桶通过严格领先门槛。
- G-C2：三者在共同资源等级下的 median、P95、吞吐和峰值 RSS 全部通过性能领先门槛。
- G-C3/G-C4：可靠性和逐格式/逐类别闸门通过，无 aggregate 掩盖的失败桶。
- 三个算法 crate 的能力清单、默认 engine Markdown 和错误契约无非预期丢失。
- 全量测试、评测原始数据、环境信息和二进制/模型哈希可复现。

在 G-S 通过但 G-C 未全部通过时，准确表述是“V2 已超过冻结旧方案；竞品矩阵中已通过 X/Y 项”，不得写成
“全面超过 LiteParse、pdf-inspector、Anydoc 等开源框架”。只有 `gate_GC.json` 全部为 `PASS` 且发布前
upstream refresh 完成后，才允许在明确版本、数据集和资源等级的限定下使用该声明。
