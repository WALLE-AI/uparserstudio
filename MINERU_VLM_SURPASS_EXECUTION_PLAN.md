# uparser mineru-vlm 全指标超过官方 MinerU 技术执行方案

> 日期：2026-08-14  
> 目标：在 OmniDocBench 同版本、同输入、同评测口径下，使 uparser mineru-vlm 的 Text、Formula、Table、Reading Order 四类指标稳定超过官方 MinerU2.5-Pro。  
> 原则：先完成同 checkpoint 官方链路对齐，再通过低置信度重识别、多模型候选和学习型选择器取得增量；禁止直接使用 OmniDocBench 测试集拟合规则或阈值。

## 1. 当前基线与目标

当前全量评测覆盖 OmniDocBench 1651 页，结果位于：

- `benchmark/OmniDocBench/result/mineru-vlm-current_quick_match_metric_result.json`
- `benchmark/OmniDocBench/result/mineru-vlm-current_quick_match_run_summary.json`

| 指标 | 当前 uparser | 官方参考 | 差距 | 项目验收线 |
|---|---:|---:|---:|---:|
| Text Edit，越低越好 | 0.0669 | 0.036 | 需降低约 46% | <= 0.034 |
| Formula | Edit 0.0967 | CDM 97.45 | 当前不可直接比较 | CDM >= 97.70 |
| Table TEDS，页面均值 | 0.9243 | 0.934 | -0.97pp | >= 0.940 |
| Table TEDS-S，页面均值 | 0.9549 | 0.959 | -0.41pp | >= 0.965 |
| Reading Order Edit | 0.1372 | 0.120 | 需降低约 12.5% | <= 0.115 |

注意：旧报告使用的 Table `0.8993/0.9293` 是逐表 raw aggregation；对官方榜单应使用 evaluator notebook/page aggregation，即 `0.9243/0.9549`。后续报告必须同时保存两种聚合值，但只用页面聚合值对榜。

## 2. 已确认的误差事实

### 2.1 Text

- 16227 个文本评测块中，448 个预测为空或完全漏匹配。
- 文本块累计 sample loss 为 1709.34，漏匹配贡献 448，约占 26.2%。
- 即使全部救回漏匹配，仍不足以将 Text Edit 从 0.0669 降到 0.036，必须继续降低已匹配正文的字符错误。
- title 平均 Edit 约 0.0385，普通 `text_block` 平均约 0.1317，资源应集中在正文、混合公式正文、报纸和复杂多栏页面。
- 已验证无条件几何段落合并会回退，因为评测器容忍连续预测块过切分，却会惩罚错误 over-merge。

### 2.2 Formula

- 当前 Formula Edit 为 0.0967，但官方使用 CDM 97.45，两个指标不能直接比较。
- 当前环境缺少完整 TeX/Ghostscript 渲染链，尚不具备“公式超过官方”的有效证据。
- 公式主要困难集中在扫描退化、中文混排、复杂数组和同页大量短公式。

### 2.3 Table

- 665 个表格样本中有 18 个完全漏匹配，是最高收益的表格修复点。
- 当前实验版 OTSL 转换相对上一全量版本影响 42 个表格，raw TEDS 平均下降约 0.00236，并出现少数灾难性退化。
- 表格结构指标距离官方只差约 0.41pp，必须优先避免错误转换，再增加漏表救援和多候选选择。

### 2.4 Reading Order

- 1638 个页面中 884 页已经为 0 Edit，即超过一半页面无需调整。
- 116 页 Edit 大于 0.5，少量灾难页贡献了主要顺序损失。
- 从 0.1372 降至 0.120，需要减少约 28.2 的累计页面损失，不需要也不应该全局重排。

### 2.5 已证伪或低收益方向

- stage-2 贪心采样对齐在现有 endpoint 上近似 no-op，只能作为协议保真项。
- 无条件识别 `list`/`equation_block` 会挤占文本匹配并扰乱阅读顺序。
- 无条件几何段落合并明确回退。
- OmniDocBench evaluator 已执行全角转半角，继续优化全半角规则基本不会提高榜单分数。
- 单纯堆叠全局后处理规则无法解释或弥补约 46% 的 Text 差距。

## 3. 总体技术架构

```text
页面图像
  |-- MinerU2.5-Pro-2605 layout
  |-- PP-DocLayoutV2 辅助检测
  `-- 类别感知框融合与漏检救援
                    |
             分类别候选生成
       |------------|-------------|
       |            |             |
   Text/OCR      Formula       Table/OTSL
   多 crop 候选   多识别器候选   多结构候选
       |            |             |
       `------ 置信度与一致性选择器 ------'
                    |
          类别专用、可回滚后处理
                    |
           选择性阅读顺序重排
                    |
          IR -> Markdown / HTML
```

增强链路必须提供两个 profile：

- `parity`：严格复刻官方 2605 推理和后处理，用于归因和回归基线。
- `enhanced`：在 parity 基础上启用候选生成、专用模型和选择器，用于超过官方。

## 4. Phase 0：评测可信化与同源基线

预计 2–3 个工程日。

### 4.1 工作项

1. 部署 `MinerU2.5-Pro-2605-1.2B`，使用官方 server launcher，确认加载 `MinerULogitsProcessor`。
2. 对同一批图片并行运行：
   - 官方 `MinerUClient + 2605`；
   - uparser `parity + 2605`；
   - uparser `enhanced + 2605`。
3. 恢复 CDM 依赖：TeX、Ghostscript、必要字体，并跑通公式 smoke test。
4. 固定模型哈希、processor 哈希、vLLM 版本、`mineru_vl_utils` 版本、服务启动参数和 evaluator commit。
5. 每次推理保存 trace manifest：
   - 原始页面哈希与尺寸；
   - stage-1 输入图像哈希、请求体、原始输出；
   - layout boxes、类别、角度、tail 和 `merge_prev`；
   - stage-2 crop 哈希、prompt、sampling、logprob、finish reason、原始输出；
   - OTSL、公式和文本后处理前后内容；
   - 最终 block IR 和 Markdown。

### 4.2 代码落点

- `uparser/crates/uparser-core/src/adapters/mineru_vlm.rs`
- `uparser/crates/uparser-core/src/transport.rs`
- `benchmark/run_uparser_omnidoc.py`
- 新增建议：`benchmark/compare_mineru_traces.py`

### 4.3 完成门槛

- CDM 能稳定产出，无环境 fallback。
- 官方和 uparser 的每个阶段都能按 image/block ID 对齐比较。
- 同一配置连续三次推理的非确定性差异可被定位到具体 block。
- 后续任何指标变化都能追溯到请求、模型原始输出或某个后处理函数。

## 5. Phase 1：官方 2605 链路严格对齐

预计 4–6 个工程日。

### 5.1 请求与服务对齐

1. 对齐官方 sampling：`temperature=0`、`top_p=0.01`、`top_k=1`、penalty 和 token 上限。
2. 将 `no_repeat_ngram_size=100` 放入 `vllm_xargs`；当前顶层字段不能作为已生效的证据。
3. 服务端显式注册 `mineru_vl_utils:MinerULogitsProcessor`。
4. 增加请求回显或服务启动审计，确认参数没有被 OpenAI compatibility layer 丢弃。
5. 对 truncation、length finish 和重复循环分别记录，禁止静默返回部分内容。

### 5.2 图像预处理对齐

1. stage-1 固定 1036x1036，使用 BICUBIC；当前 Rust 实现使用 CatmullRom。
2. stage-2 极端比例 padding、短边放大全部按官方 `ceil` 规则。
3. 对旋转块逐像素比较 Python PIL 与 Rust 输出。
4. crop 边界、浮点转整数和越界 clamp 必须建立 golden fixture。

### 5.3 layout 与类别对齐

1. 使用 DOTALL 正则完整解析多个 box、旋转 token 和 box 后 tail。
2. 解析 `txt_contd_tgt`，但只保存为 `merge_hint`，不得立即无条件合并。
3. 对齐 `unknown -> image`、`inline_formula` 跳过和非法 bbox 规则。
4. 对齐 table internal block、image caption containment 和 image block 规则。
5. 对齐 `list/list_item/equation_block/image/chart` 的抽取与 skip 行为。

### 5.4 后处理对齐

1. 移植官方公式修复链和 text display-to-inline 修复。
2. 对齐 `[Non-Text]` 清理、paratext、list item 和 equation block 处理。
3. 对齐 image/chart 内容再分类，但独立开关，先统计频次和收益。
4. OTSL 使用官方 Python 实现作为 oracle：
   - 从真实输出采集至少 5000 条 OTSL；
   - 加入随机合法/残缺 OTSL property tests；
   - Rust HTML 经 DOM canonicalization 后必须与 Python 结果一致；
   - 不允许只用字符串样例验证 rowspan/colspan。
5. 当前实验性 OTSL、表内过滤、paratext 丢弃分别设 feature/profile 开关，任何一项都可单独关闭。

### 5.5 完成门槛

- Python/Rust crop 像素一致或差异有明确容差证明。
- 合法 OTSL golden fixture DOM 一致率 100%。
- uparser parity 与官方 2605 的 block 数、类别、内容和顺序差异全部有归因报告。
- 全量 1651 页 parity 指标不能低于官方同 checkpoint 结果的统计置信下界。

## 6. Phase 2：Text 低置信度重识别

预计 5–8 个工程日，是 Text 超过官方的核心阶段。

### 6.1 置信度特征

每个正文块记录：

- 平均 logprob、最低 logprob、logprob 标准差；
- 输出长度与 bbox 面积、行数的比例；
- 重复 n-gram 比例；
- Unicode script 与页面语言一致性；
- VLM 与专用 OCR 的字符一致率；
- 是否包含异常 `[Non-Text]`、乱码、截断和未闭合公式；
- bbox 是否过紧、跨栏或与其它容器冲突。

### 6.2 候选生成

只对低置信度正文块生成额外候选：

1. 原始 crop。
2. bbox 外扩 2% 和 4%。
3. 1.5x 和 2x BICUBIC 放大。
4. 原始 RGB 与灰度/轻对比增强版本。
5. MinerU VLM text recognition。
6. PaddleOCR/PP-OCR 专用文本识别。
7. 含行内公式时，增加“文本 OCR + 公式识别 + 几何拼接”候选。

候选数量必须受预算限制：高置信度块只跑一次，低置信度块最多增加 2–3 次调用。

### 6.3 候选选择器

先实现规则 scorer，再训练轻量 GBDT/MLP ranker：

- 输入：模型 logprob、OCR/VLM agreement、语言一致性、字符覆盖率、重复率、crop 质量和页面类型。
- 输出：每个候选成为最优文本的概率。
- 训练标签：外部文档真值或源 PDF 文本层，不使用 OmniDocBench 1651 页 GT。
- 选择器必须允许“保留官方候选”，避免增强系统强制替换正确结果。

### 6.4 文本验收门槛

- 先达到 Text Edit <= 0.040，再决定是否进入 LoRA 阶段。
- 最终目标 <= 0.034。
- title、英文、中文、报纸、三栏、公式混排分别报告，任何主要子集不得显著回退。
- 448 个漏匹配块至少救回 60%，同时不能通过大规模错误新增块扰乱 Reading Order。

## 7. Phase 3：Table 超越方案

预计 4–6 个工程日。

### 7.1 漏表救援

1. MinerU layout 与 PP-DocLayoutV2 并行输出 table boxes。
2. 对 MinerU 未检出、辅助 detector 高置信度检出的区域发起 table recognition。
3. 用包含关系、文字密度、规则线和表格分类器过滤误检。
4. 优先覆盖当前类型中的无框表、扫描表、跨栏表和报纸小表。

### 7.2 多结构候选

每个疑难表格生成：

- MinerU OTSL 原 crop；
- MinerU OTSL padding crop；
- SLANet-Plus；
- Unet wired-table；
- 必要时 OCR cell text 回填候选。

### 7.3 TEDS proxy selector

候选特征：

- HTML 是否可解析；
- 行列闭合、span 可达和覆盖完整性；
- 空单元格率、异常超长单元格率；
- OCR 文本与 cell 文本一致率；
- 模型 logprob 与 OTSL token 合法率；
- 有线/无线表格分类结果；
- 多候选行列数一致性。

训练一个 TEDS proxy ranker，预测候选结构质量。禁止根据 OmniDocBench GT 在运行时选择候选。

### 7.4 表格验收门槛

- 18 个完全漏表样本类型的外部回归集 rescue recall >= 80%。
- OTSL converter 不允许出现单个改动导致 TEDS 从 >0.8 降到 0 的灾难案例。
- 全量页面 TEDS >= 0.940，TEDS-S >= 0.965。
- Table 增强不得使 Text 或 Reading Order 超过 0.001 的回退。

## 8. Phase 4：选择性 Reading Order 修复

预计 4–6 个工程日。

### 8.1 页面异常检测

仅在以下信号明显时进入重排：

- 同栏中出现大范围 y 方向逆序；
- 顺序在左右栏间频繁跳跃；
- title 出现在其正文之后；
- caption 与最近 image/table 被其它栏内容隔断；
- footer/page number 插入正文中部；
- 模型顺序与 XY-cut 顺序存在大量高置信度冲突。

### 8.2 Pairwise ranker

为任意两个 block 预测 `A before B`：

- 几何位置和重叠；
- 所属列、跨栏状态；
- block 类别；
- 包含与 caption 归属关系；
- 模型原始顺序；
- XY-cut 顺序；
- 页面版式类别。

将 pairwise 分数转换为带约束的 DAG，再做稳定拓扑排序。高置信度模型顺序作为软约束，而不是全部覆盖。

### 8.3 顺序验收门槛

- 已经 0 Edit 的 884 页至少 99.5% 保持不变。
- 优先降低 Edit > 0.5 的 116 页。
- 全量 Reading Order <= 0.115。
- 不接受“平均分变好但完美页面大量退化”的方案。

## 9. Phase 5：Formula CDM 超越

预计 4–8 个工程日。

### 9.1 候选模型

- MinerU VLM Formula Recognition。
- UniMERNet。
- PP-FormulaNet Plus-M。
- 对复杂数组公式增加 padding/放大 crop 候选。

### 9.2 选择策略

1. 过滤未闭合 brace、非法环境和明显截断候选。
2. 对候选 LaTeX 做规范化，但保留原始版本用于回滚。
3. 将候选渲染为图像，与原公式 crop 比较视觉结构。
4. 综合识别 logprob、渲染成功率、视觉相似度和候选一致性选择。
5. 对 equation block 使用结构感知合并，不把额外公式块直接插入正文序列。

### 9.3 公式验收门槛

- CDM 工具链三次重复评测结果稳定。
- Formula CDM >= 97.70。
- Formula 增强不得使 Text Edit 或 Reading Order 回退超过 0.001。

## 10. Phase 6：必要时进行分阶段 LoRA

只有满足以下条件才进入训练：

- 2605 parity 已完成；
- Text 经过多候选后仍 > 0.040；
- 错误主要来自模型识别，而不是 layout、crop 或匹配。

不得训练一个共享 LoRA 同时处理所有 prompt。建议分别训练：

- layout LoRA；
- text recognition LoRA；
- formula recognition LoRA；
- table OTSL LoRA。

推理时根据 prompt 加载对应 adapter。训练数据来自外部真实文档、源 PDF 文本层、合成多栏文档、公式数据、PubTabNet/FinTabNet 类表格数据。OmniDocBench 测试集只能做最终盲测。

## 11. 代码组织建议

### 11.1 配置

新增 `MineruVlmProfile`：

```rust
enum MineruVlmProfile {
    Parity,
    Enhanced,
}
```

所有实验功能使用显式配置，不读取散落的临时环境变量。建议参数包括：

- `enable_layout_rescue`
- `enable_text_candidates`
- `enable_formula_candidates`
- `enable_table_candidates`
- `enable_order_reranker`
- `enable_image_analysis`
- `trace_dir`
- 各类别置信度阈值与最大候选预算

### 11.2 模块

建议新增：

- `adapters/mineru_vlm/profile.rs`
- `adapters/mineru_vlm/trace.rs`
- `adapters/mineru_vlm/confidence.rs`
- `adapters/mineru_vlm/candidates.rs`
- `adapters/mineru_vlm/selectors.rs`
- `adapters/mineru_vlm/order_ranker.rs`

OTSL、公式和通用 geometry 仍可保留共享模块，但 MinerU 特有规则不得无条件影响其它协议。

## 12. 实验矩阵与晋级规则

每项功能必须独立运行，推荐实验命名：

| 实验 | 内容 |
|---|---|
| E0 | official-2605 |
| E1 | uparser-parity-2605 |
| E2 | E1 + exact OTSL |
| E3 | E2 + layout rescue |
| E4 | E3 + text candidates |
| E5 | E4 + table candidates |
| E6 | E5 + selective order ranker |
| E7 | E6 + formula candidates |
| E8 | E7 + stage-specific LoRA，如确有必要 |

晋级规则：

1. 快速开发集只能筛掉明显坏方案，不能证明方案有效。
2. 看起来正向的功能必须在自然分布全量集复核。
3. 四项主指标中任何一项显著回退，功能不能进入默认 enhanced profile。
4. 最终结果至少运行三次，报告均值、标准差和 bootstrap 95% CI。
5. 必须保留逐页、逐块 delta，避免平均分掩盖灾难案例。
6. opendataloader-bench 和 workspace tests 作为生产回归护栏同步执行。

## 13. 测试要求

每次 Rust 变更至少执行：

```bash
cargo fmt --all -- --check --manifest-path uparser/Cargo.toml
cargo test --workspace --manifest-path uparser/Cargo.toml
cargo clippy --workspace --all-targets --manifest-path uparser/Cargo.toml -- -D warnings
```

外加：

- 官方 Python 与 Rust OTSL differential tests；
- PIL/Rust crop pixel comparison；
- 请求 JSON golden tests；
- layout token parser fuzz/property tests；
- selector 离线准确率、校准度和 fallback 测试；
- 完美顺序页面保持率测试；
- 真实 endpoint smoke、200 页开发集、1651 页最终全量评测。

## 14. 回滚策略

- `parity` 永远保持可运行，作为增强功能回滚目标。
- 每个增强项有独立配置位，禁止通过回退整个 adapter 关闭单项。
- trace manifest 记录每个 block 选择了哪个候选以及原因。
- selector 置信度不足时必须回退官方 MinerU 候选。
- OTSL 转换失败或结构 validator 不通过时保留旧候选，不输出空表。
- order ranker 不能生成稳定 DAG 时保留模型原始顺序。

## 15. 里程碑

| 里程碑 | 预计时间 | 交付物 | 退出条件 |
|---|---:|---|---|
| M0 可信基线 | 2–3 天 | 2605、CDM、trace、双跑报告 | 指标和中间结果可复现 |
| M1 官方 parity | 4–6 天 | parity profile、差分测试 | 同 checkpoint 接近官方 |
| M2 Text 突破 | 5–8 天 | OCR/VLM 候选与 selector | Text <= 0.040 |
| M3 Table 超越 | 4–6 天 | 漏表 rescue、TEDS proxy | TEDS >= 0.940 |
| M4 Order 超越 | 4–6 天 | selective order ranker | RO <= 0.115 |
| M5 Formula 超越 | 4–8 天 | CDM 候选选择 | CDM >= 97.70 |
| M6 最终盲测 | 2–3 天 | 三次全量评测报告 | 四项同时超过官方 |

总投入预计 4–7 个工程周；如果 2605 parity 已经明显改善 Text，可能缩短。如果仍必须训练四类 LoRA，则增加约 1–3 周。

## 16. 最终完成定义

只有同时满足以下条件，才能对外声明“全面超过官方 MinerU”：

1. 使用相同的 MinerU2.5-Pro-2605 checkpoint 或明确列出模型差异。
2. 使用官方一致的 OmniDocBench commit、输入和指标聚合方式。
3. Text Edit <= 0.034。
4. Formula CDM >= 97.70。
5. Table TEDS >= 0.940，TEDS-S >= 0.965。
6. Reading Order Edit <= 0.115。
7. 三次全量运行稳定，且 95% CI 支持超过官方而不是单次波动。
8. 未使用 OmniDocBench 测试 GT 调参、训练或运行时选候选。
9. workspace tests、opendataloader-bench 和真实文档回归全部通过。

## 17. 立即执行的前三项

1. 启动并验证官方 2605 endpoint、`MinerULogitsProcessor` 和 `vllm_xargs`。
2. 建立 official/uparser 双跑 trace，先回答当前 Text 差距中有多少来自 2604 checkpoint、有多少来自 uparser 链路。
3. 将当前 OTSL、表内过滤、paratext 和 normalization 实验改为独立 profile 开关，恢复稳定 parity 基线后再逐项 A/B。

这三项完成前，不继续增加新的全局后处理规则。

## 18. 2026-08-14 执行状态：2605 endpoint 与首轮全量基线

### 18.1 已完成

1. 已使用以下核心参数启动并保持官方 2605 endpoint：
   - checkpoint：`MinerU2.5-Pro-2605-1.2B`
   - endpoint：`http://127.0.0.1:19122/v1/chat/completions`
   - `--max-model-len 8192`
   - `--logits-processors mineru_vl_utils:MinerULogitsProcessor`
2. `/v1/models` 返回 HTTP 200，模型 ID、checkpoint root 和 `max_model_len=8192` 均正确。
3. 服务端环境确认为 `vllm 0.19.0`、`mineru-vl-utils 0.1.14`；另用官方
   `mineru-vl-utils 1.0.5` 的 `MinerUClient(http-client)` 对同一 endpoint 完成烟测。
4. 已验证 `vllm_xargs={"no_repeat_ngram_size": 2, "debug": true}` 请求返回 HTTP 200。
   对比 `mineru-vl-utils 0.1.14` 与 `1.0.5` 的 no-repeat-ngram 实现后，确认核心算法和
   extra-args 协议一致，差异仅为调试输出方式。
5. uparser 已将 `no_repeat_ngram_size=100` 从无效的顶层请求字段迁移到
   `vllm_xargs`，并增加请求 JSON 单元测试，防止协议回退。
6. benchmark runner 已支持受控并发、失败不落完成文件、非空完成检查、超时进程清理和
   Ctrl-C 收敛。本轮使用 `--workers 2`，生成与 endpoint 均稳定完成。
7. OmniDocBench 1651 页全量匹配完成；page match、TEDS 均无 timeout/error。
8. 沙箱外执行全 workspace tests 通过：uparser-core 307、CLI 29、contract 2、
   native-engine 755、doc tests 2，合计 1095 项通过、0 失败。

### 18.2 当前 2605 实测结果

本轮名称为 `mineru-vlm-2605-current`。它包含当前工作树中的 OTSL、表内过滤和渲染实验，
因此是 **current-enhanced 基线**，不是官方 parity 基线，不能用于拆分官方链路差异。

| 指标 | uparser 2605 current | 官方参考 | 差距 | 是否超过 |
|---|---:|---:|---:|---|
| Text Edit，越低越好 | 0.06820 | 0.036 | +0.03220 | 否 |
| Formula Edit，越低越好 | 0.09989 | 非官方主指标 | - | 不可比 |
| Formula CDM，越高越好 | 未启用 | 97.45 | - | 未验证 |
| Table TEDS，页级，越高越好 | 0.92484 | 0.934 | -0.00916 | 否 |
| Table TEDS-S，页级，越高越好 | 0.95441 | 0.959 | -0.00459 | 否 |
| Reading Order Edit，越低越好 | 0.13868 | 0.120 | +0.01868 | 否 |

结论：**endpoint、`MinerULogitsProcessor` 和 `vllm_xargs` 已正确接通，但当前实现尚未全面
超过官方 MinerU。** 当前主要差距按优先级为 Text、Reading Order、Table，Formula CDM
尚缺正式评测，不能声明超过。

预测文件中有 1647 个非空 Markdown 和 4 个空输出。其中 1 页 GT 本身无有序内容，另外
3 页是整页图片或图片加少量标题；这暴露了 `--no-assets` 评测路径对 image-only 页面无法
表达的覆盖盲点。后续 runner 应把“命令成功但输出为空”区分为合法空页和异常空页，并生成
单独清单，不能只按退出码统计成功。

### 18.3 下一步执行顺序

1. 冻结当前结果为 E0-current，创建真正的 `uparser-parity-2605` profile：严格复刻官方
   resize、crop、prompt、采样、类别跳过、OTSL 和 Markdown 规则，关闭当前所有增强实验。
2. 建立 official-client 与 uparser 的逐页双跑 trace，至少保存 stage-1 layout token、
   每个 crop、stage-2 原始响应、最终 block/Markdown 和逐页 metric delta。
3. 先完成 Text gap attribution：按漏框、错框、crop 偏移、OCR 候选、后处理丢字、
   image-only 空输出分类，目标是解释不少于 95% 的 Text 总损失。
4. 恢复并接入官方 CDM evaluator；CDM 未出结果前，Formula 方向不得晋级或对外宣称超越。
5. parity 误差收敛后，再按 E2-E7 顺序逐项 A/B；每项必须同时报告四项主指标和逐页退化集，
   禁止把多个增强一次性混入全量评测。
6. 达到第 16 节门槛后执行三次全量盲测和 bootstrap 95% CI，最后才判断是否全面超过官方。

## 19. 2026-08-14 执行状态：官方等价 profile 与完整 A0 基线

### 19.1 已实现

1. 新增独立协议 `mineru-vlm-official`，默认 `mineru-vlm` 保持兼容：
   - 使用官方严格 whole-response layout grammar；
   - 越界坐标直接丢弃，不执行 clamp/rescue；
   - `unknown -> image`，跳过 `inline_formula` 和未知类型；
   - 关闭 uparser 的 IoU 去重和退化文本升温重试；
   - 使用官方 stage-2 skip list；
   - 自动禁用通用几何段落合并；
   - Markdown 按官方 OmniDocBench 脚本原样连接 block content，不添加标题、列表或二次公式包装。
2. 请求补齐 `repetition_penalty=1.0`，`no_repeat_ngram_size=100` 继续通过
   `vllm_xargs` 发送。
3. endpoint 进程命令行已确认包含
   `--logits-processors mineru_vl_utils:MinerULogitsProcessor`；`/v1/models` 和真实图片
   两阶段请求均成功。
4. 新增严格 parser、profile 注册和官方 Markdown renderer 单测。宿主网络环境执行全 workspace
   tests 通过，合计 1105 项，0 失败；沙箱内 localhost wiremock 会被代理改写，不能作为失败依据。
5. 完成 1651 页 `mineru-vlm-2605-official` 全量预测和官方 evaluator：预测 1651/1651，
   runner 无错误日志，TEDS 665 个样本无 timeout/error，page match 有 2 个 quick-match timeout。

### 19.2 A0 官方等价基线结果

| 指标 | uparser current | uparser official profile | 官方参考 | official profile 相对 current | 是否超过官方 |
|---|---:|---:|---:|---:|---|
| Text Edit | 0.06820 | **0.03773** | 0.036 | -0.03047 | 否，差 0.00173 |
| Formula Edit | 0.09989 | **0.09354** | 非官方主指标 | -0.00635 | 不可据此判断 CDM |
| Formula CDM | 未启用 | 未启用 | 97.45 | - | 未验证 |
| Table TEDS | 0.92484 | **0.92676** | 0.934 | +0.00192 | 否，差 0.00724 |
| Table TEDS-S | 0.95441 | **0.95632** | 0.959 | +0.00192 | 否，差 0.00268 |
| Reading Order Edit | 0.13868 | **0.12962** | 0.120 | -0.00906 | 否，差 0.00962 |

这次结果证明此前最大问题不是 checkpoint 或 endpoint，而是 uparser 通用渲染/后处理偏离官方评测
契约。仅恢复官方等价行为，Text 已回收约 94.6% 的原始差距，Reading Order 回收约 48.5%，
Table 也小幅改善。但四项仍未全面超过官方，禁止对外声明已超过。

### 19.3 页面级差分结论

official profile 相对 current：

| 指标 | 改善页/表 | 回退页/表 | 不变页/表 |
|---|---:|---:|---:|
| Text | 344 | 53 | 1160 |
| Reading Order | 197 | 61 | 1380 |
| Table TEDS | 19 | 17 | 629 |

两个最大的 Text 和 Reading Order 回退来自同两页长索引：

- `docstructbench_enbook-zlib-o.O-17208435.pdf_105.jpg`
- `docstructbench_enbook-zlib-o.O-17208435.pdf_57.jpg`

official profile 分别输出约 381/401 行并触发本轮仅有的两个 quick-match timeout，Text/RO 均记为
1.0；旧通用几何合并输出 5 行，Text 约 0.0044/0.0069、RO 为 0。仅修复这两个可泛化的长索引
长尾，Text 预计可从 0.03773 降至约 0.03645，几乎解释当前 Text 与官方参考的全部差距；但 RO
仍有约 0.0084 的真实结构差距，不能靠 timeout 修复解决。

### 19.4 下一轮执行优先级

1. **E1-safe-index-merge**：只对高度规则、短行密集、同列连续的索引型文本启用线性合并；禁止
   恢复全局 N1。两页离线验证已通过，下一步跑全量，要求 Text 与 RO 同时不回退。
2. **PIL/Rust 像素对齐**：建立官方 PIL 与 Rust 的 layout resize、float bbox crop、rotate、padding、
   BICUBIC 像素 differential；逐项消除 crop/hash 差异后在 200 页冻结开发集验证。
3. **E2-exact-OTSL**：用 665 个真实 table raw response 做官方 1.0.5 `convert_otsl_to_html` 与 Rust
   differential，按 token parser、span、HTML canonicalization 分类。目标先补足 TEDS 0.00724。
4. **E3-order-selective**：对 RO 回退页提取 category/bbox 序列，只在多列/跨栏置信度高时重排；
   单列和模型原顺序正确页必须原样保留。目标 RO <= 0.115。
5. **trace 基础设施**：后续基准必须保存 stage-1 raw token、block bbox/category、stage-2 raw、最终
   block 和 selector decision，避免每次只保存 Markdown 后被迫重新推理。
6. **CDM 恢复**：安装或固定官方 CDM 依赖并产出正式 CDM；在此之前公式只报告 Edit，不能进入
   “全面超过”验收。

下一轮不得把上述 E1-E3 合并上线。每项用独立预测名和逐页 delta 晋级，最终仍按第 16 节门槛
和三次全量运行验收。

### 19.5 E1-safe-index-merge 实施与两页验证

已新增隔离协议 `mineru-vlm-surpass`。它继承 official profile 的 parser、采样参数、stage-2 行为和
Markdown 契约，只在同时满足以下条件时执行几何合并：

1. 页面 block 数不少于 150；
2. text block 占比不少于 90%；
3. 至少 90% 的 text block 有 bbox、非空且不超过 100 字符；
4. 相邻 text block 中至少 75% 满足同列、垂直连续的合并条件。

两个超时页 trace 分别包含 187/197 个 text block，平均文本长度 29.2/32.2，均稳定分为三列，
命中选择器后 Markdown 从 381/401 行降为 13/13 行。使用相同官方 evaluator 对原始 GT 两页子集
评测：

| 指标 | official profile 两页 | E1 两页 | 变化 |
|---|---:|---:|---:|
| quick-match timeout | 2 | 0 | -2 |
| Text Edit | 1.0 | 0.00562046 | 显著改善 |
| Reading Order Edit | 1.0 | 0.0 | 改善至满分 |

保持其余 1649 页完全不变并按官方基线分母换算，E1 预期为 Text `0.03645036`、RO
`0.12840077`。独立完整 1651 页 E1 run 已完成，正式结果为 Text `0.03673060`、Formula Edit
`0.09483955`、TEDS `0.92605025`、TEDS-S `0.95555340`、RO `0.12845440`。page/quick-match
timeout 均为 0，665 个 TEDS 样本无 timeout/error/exception。

相对 official profile，Text 改善 `0.00099706`、RO 改善 `0.00116737`，但 Formula Edit 回退
`0.00130229`、TEDS 回退 `0.00071107`、TEDS-S 回退 `0.00077129`。两次独立推理之间有 380 个
Markdown 文件字节不同，说明 greedy + 四路并发仍存在 run-to-run 方差；逐页 Text 为 20 改善、
39 回退、1498 不变，RO 为 9 改善、7 回退、1622 不变。E1 的两页因果增益成立，但完整 run
不满足所有指标不回退，当前仍禁止声明全面超过。下一步进入 PIL/Rust 像素 differential，并把
推理复现方差纳入后续实验的对照设计。

### 19.6 PIL/Rust 像素 differential 执行结果

已新增官方 PIL / Rust production-code probe，对同一真实 JPEG 的 layout resize、浮点 bbox crop、
旋转、极端比例 padding 和短边 upscale 做逐像素比较。初始结果：layout 差异像素 7.318%、MAE
0.1211/255；普通 0° crop 差异 0.791%、MAE 0.0079；极端比例 padding 逐像素完全一致。

发现并修复一个高影响方向错误：Pillow 正角度旋转为逆时针，Rust `imageops::rotate90` 为顺时针。
90° extract 修复前差异像素 51.028%、MAE 38.0885、最大通道差 255；修复后降为 0.791%、MAE
0.0079、最大通道差 1。另将 MinerU bbox 和短边 resize 尺寸取整改为 half-to-even，并增加单测。

像素对齐修复尚未进入独立指标 run，暂不声明精度收益。下一步应先在含 rotate token 的冻结页面
集验证 Text/Formula 不回退，再进入 exact OTSL differential；layout 的低幅重采样差异单独保留，
禁止为追求 hash 一致引入 Python/Pillow 运行时依赖。
