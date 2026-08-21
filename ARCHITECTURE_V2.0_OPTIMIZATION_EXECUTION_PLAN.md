# UParser V2 超越旧方案优化执行计划

> 状态：待执行  
> 制定日期：2026-08-21  
> 对应架构：`ARCHITECTURE_V2.0_PROPOSAL.md` §1.1 G-S、§12 V11  
> 目标：在保留 `uparser-native-engine`、`uparser-document-engine` 和 `uparser-core` 既有文档算法的前提下，使 V2 在精度、性能和工程可信度上形成相对旧方案的可复现 Pareto 改进。

## 1. 当前基线与差距

| 维度 | 当前结果 | 发布要求 | 差距 |
|---|---:|---:|---|
| 三个核心包行覆盖率 | 86.43% | ≥90% | 至少新增约 2202 个有效覆盖行（分母不变时） |
| `uparser-core` 行覆盖率 | 91.08% | ≥92% | 约 95 行 |
| `uparser-document-engine` 行覆盖率 | 83.73% | ≥88% | 约 357 行 |
| `uparser-native-engine` 行覆盖率 | 85.83% | ≥90% | 约 1790 行 |
| Native Bench A Overall | 0.875425 | 不低于旧方案且产生严格改进 | 当前仅完全保真 |
| Native 性能 | 0.050810 s/doc | ≤0.047341 s/doc | 慢 3.47 ms/doc，约 7.33% |
| MinerU Bench A Overall | 0.923978 | ≥0.928368，且同 checkpoint A/B 不退 | 回退 0.004390，当前比较还存在 checkpoint 混淆 |
| Auto 相对 Native Overall | +0.016598 | ≥+0.02 | 差 0.003402，且缺人工 G-R |
| OmniDocBench | 文本/公式/顺序及两项表格均弱于指定历史基线 | 五项不退，至少一项改善，空白输出为 0 | 未通过 |

当前工作的重点不是继续抽象 runner，而是用覆盖率和逐样本评测定位高风险代码，做定向测试、剖析和算法优化。

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
W6 CI 固化与全量 G-S 验收
```

W1 的测试资产可与 W0 的工具建设并行，但任何性能或精度改动必须在 W0 完成后开始。

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

## 5. W1：覆盖率驱动的风险补测

### 5.1 P0 模块与测试矩阵

| 模块 | 当前 | 目标 | 必须新增的场景 |
|---|---:|---:|---|
| `extractor/xobjects.rs` | 6.80% | ≥80% | Image/Form XObject、嵌套 Form、父子 Resources、局部 Matrix、循环引用、缺失对象、不同 Filter、Form 内字体 |
| `tounicode.rs` | 53.70% | ≥85% | bfchar/bfrange、usecmap、Identity-H/V、CIDToGIDMap、W 数组、内置 CMap、Type0/simple font、坏 stream、代理对和多字符映射 |
| `formats/sheet.rs` | 0% | ≥85% | XLS/XLSX/ODS、公式、日期/数字、合并单元格、多 Sheet、隐藏 Sheet、空行、链接和资源预算 |
| `formats/doc.rs` | 58.19% | ≥80% | OLE/FIB 边界、编码、复杂表格、嵌入对象、损坏 stream、加密和部分恢复 |
| Native `lib.rs` | 68.53% | ≥80% | detect/full/pages/password/options、损坏 PDF、页面过滤、扫描/混合、部分页错误、Markdown 选项 |

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

- 五轮 median `≤0.047341 s/doc`，P95 不高于旧方案。
- Native 四项质量不下降，逐文件非预期差异为 0。
- 每文档 PDF/结构化解析次数为 1，峰值 RSS 不增加超过 5%。

## 7. W3：提升 Native 与结构化格式精度

### 7.1 优化顺序

1. **XObject 文本完整性**：修复嵌套 Form 的资源继承、变换矩阵、字体和阅读顺序；直接影响工程规范、表单和复杂 PDF。
2. **ToUnicode 与字体恢复**：按 CMap 来源建立置信级别，优先权威 ToUnicode，再使用 CID/GID/字体 cmap fallback；禁止低置信 fallback 覆盖高置信映射。
3. **Native 表格与标题**：只针对 Bench A 逐样本失败簇优化，重点提升 TEDS/MHS，不全局调阈值。
4. **结构化 Sheet/DOC**：保留 Sheet 名、merge、类型、公式、link、heading/table 和 warning；以源语义为准，不经 PDF 中转。
5. **Canonical renderer**：继续保持非默认；只有达到 engine 的 Overall/NID/TEDS/MHS 和 golden 后才重新评估切换。

### 7.2 候选变更晋级规则

| 阶段 | 数据 | 通过条件 |
|---|---|---|
| 单元/fixture | P0 测试集 | 目标问题修复，既有用例全部通过 |
| Smoke | Bench A 固定 20 篇失败簇 + 20 篇随机护栏 | 目标分项改善，其他分项无回退 |
| 完整 Bench A | 200 篇 | NID/TEDS/MHS 均不退，Overall 严格提升 |
| 多格式 | 16 格式真实矩阵 | 适用语义字段全部通过，无新增失败 |

### 7.3 退出条件

- Native Bench A 四项均不低于冻结值，Overall 或至少一个关键分项形成可复现严格提升。
- Sheet/DOC 真实 fixture 端到端验证通过，格式矩阵不再只验证“能够检测和返回文本”。
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

## 10. W6：CI、发布与外部模式验收

### 10.1 CI 分层

| 层级 | 每次提交 | 每日/专机 | 发布前 |
|---|---|---|---|
| Rust tests | baseline/native/workspace/pdfium contract | 全 feature + mutation/fuzz seed | 全量零失败 |
| Coverage | 三核心包 + 变更行门槛 | P0 文件明细 | G-S3 全通过 |
| Quality | golden + 20 篇 Bench A smoke | Bench A 200 篇 | Bench A + OmniDocBench + G-R |
| Performance | Native microbench | 五轮 Native A/B | Native 与 Model-protocol 五轮 A/B |
| Formats | 16 格式受控 fixture | 真实格式矩阵 | 全字段保真报告 |

### 10.2 外部模式

- `paddlex-structure` 和 Mode 3 pipeline 在专用服务可用后分别运行协议契约、Bench A、性能和失败注入。
- 未通过真实服务闸门的模式只能显式选择，不能进入 Auto 默认候选。
- 如果发布时服务仍不可用，报告必须明确为“V2 核心路径通过，外部模式未验收”，不能宣称所有模式领先。

### 10.3 最终产物

- `COVERAGE_REPORT.md`：更新后的整体、分包和关键文件覆盖率。
- `BENCHMARK_REPORT.md`：Bench A、OmniDocBench、Auto 和外部模式的最终对比。
- `ARCHITECTURE_V2.0_EVALUATION_REPORT.md`：构建、环境、哈希、逐样本产物和 G-S 判定。
- `benchmark/results/`：机器可读原始 JSON、baseline manifest 和差值数据。
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
| M1：覆盖率 P0 | 8–12 人日 | G-S3 达标，真实/畸形 fixture 完整 |
| M2：Native 性能 | 3–6 人日 | 消除 7.33% 架构税且质量不退 |
| M3：Native/结构化精度 | 5–10 人日 | Bench A 严格改善，Sheet/DOC 字段保真 |
| M4：MinerU/OmniDocBench | 5–10 人日 + GPU 评测时间 | 五项不退且至少一项改善 |
| M5：Auto/G-R | 6–12 人日 + 标注时间 | regret、质量增益和速度均达标 |
| M6：发布验收 | 2–4 人日 + 全量运行时间 | G-S 全绿、报告与哈希固化 |

总量约 31–58 人日，实际取决于 ToUnicode/XObject 失败簇、人工标注规模和模型服务稳定性。覆盖率、Native
性能和同配置 MinerU A/B 是关键路径，应先完成；在它们未通过前，不启动大规模 prompt 或路由调参。

## 13. 完成定义

只有同时满足以下条件，执行计划才可标记完成：

- G-S1：Native、MinerU、OmniDocBench、Auto 的质量门槛全部通过。
- G-S2：Native 架构税消除，Model-protocol 同服务开销达标。
- G-S3：整体、分包、P0 文件和变更行覆盖率全部达标。
- G-S4：16 格式真实端到端语义矩阵和文档类型 G-R 冻结集通过。
- 三个算法 crate 的能力清单、默认 engine Markdown 和错误契约无非预期丢失。
- 全量测试、评测原始数据、环境信息和二进制/模型哈希可复现。

在此之前，准确表述只能是“V2 架构收敛完成，部分质量与性能闸门待优化”，不得写成“V2 已超过旧方案”。
