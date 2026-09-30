# uparser V2 架构不足 · 可执行改造方案

> 基线：`feat/architecture-v2` @ `1fba95f`（工作树，`cargo fmt --check` 干净）。
> 本文把 `ARCHITECTURE_V2.0_EVALUATION_REPORT.md` 的实测结论、
> `CORE_ARCHITECTURE_REVIEW_AND_REFACTOR_PLAN.md` 的残余项、以及本次对代码的复核
> 收敛为四条工作流（A/B/C/D），每条都给出文件:行、改动、验收、风险与工作量。
>
> 与旧计划的关系：R0/R1 已落地（统一 `runner.rs`、`execution` 缓存指纹、
> `document_profile`/`model_endpoint`/`route_decision` 已填充、结构化 lane 已前置），
> 因此本文**不重复**那些任务，只处理仍然成立的部分。

---

## 0. 前置：度量环境与铁律

任何带"精度"字样的验收都必须在下列条件下取数，否则不得写入结论：

| 项 | 要求 | 依据 |
|---|---|---|
| 代理 | `NO_PROXY=127.0.0.1,localhost` | 否则本地 wiremock/端点被企业代理拦截，表现为"代码 bug" |
| 缓存 | 全部 `--no-cache` | 缓存命中会把编排改动隐形 |
| 数据集 | OmniDocBench **全 1651 页**；ODL **全 200 篇** | `BENCHMARK_DEV_LOG.md` §3.3：290 页子集曾给出与全集相反的结论 |
| 图片列表 | 同时匹配 `*.png` 与 `*.jpg` | 只取 png 会静默漏掉 981 页 |
| CDM | `run_eval_deep.py`，TeX 根到 `texlive/2026` | 根路径写浅一层会让 CDM 全 0 而不报错 |
| 模型漂移 | 记录 checkpoint 名 | `19122` 现在是 `MinerU2.5-Pro-2605-1.2B`，冻结基线是 `2604`，不可混比 |

**铁律**：模型推理以外的一切（编排、后处理、装配、渲染）留在 Rust；Python 只做单 stage 推理
（沿用 `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md` §0.5 的边界）。

---

## A. 表格保真：把"每协议特例"变成"可度量的渲染策略"

### A.0 现状复核（修正了我先前的描述）

不是"所有 HTML 表格都被降级成 pipe 表"。实际链路是：

1. `ascend.rs:466 parse_html_table` **已经**能把 `rowspan/colspan` 还原成 canonical `Table`；
2. `uparser-document-engine/src/render/mod.rs:154` 在 `table.has_spans()` 时走
   `render_html_table`（保留 span），否则走 `render_markdown_table`（GFM pipe）；
3. `escape_table_cell` 已处理 `|` 与换行（→`<br>`）。

所以**真实残余损失**只有三处，且每处都可独立度量：

| 编号 | 损失 | 位置 | 可观测后果 |
|---|---|---|---|
| A-L1 | 无 span 的表被重排成 GFM，单元格内联标记被 `plain_cell_text` 拍平 | `render/mod.rs:510,586` | monkeyocr-v2 实测 Table Edit 0.3967→0.1040 的主要来源 |
| A-L2 | `parse_html_table` 失败即整表降级为 `strip_tags` 段落 | `ascend.rs:261-273` | 已 warn，但表格结构全丢 |
| A-L3 | monkeyocr-v2 用"整份文档自装配"来绕开 A-L1，把表格保真和文档装配两件事捆在一起 | `render/mod.rs:69`、`monkeyocr_post.rs:496,556` | 其他 HTML-表格协议（mineru-vlm / navidc-ocr / pipeline）拿不到同样收益 |

> **进度（2026-09-30）**：A.1 / A.2 已实现（代码 + 单测 + 真实二进制验证）；
> A.3 的全量评测**被环境阻塞**，详见下方 A.3。一处与计划不同的实测发现：
> `native` 的表格块也带 `html`（`adapters/native.rs` 为 IR 构造的），所以
> `--table-format source-html` 对 native 同样会改变输出——A.3 的度量矩阵比原计划
> 设想的"4 个 HTML 协议"多一列。

### A.1 T-A1 · 在 canonical `Table` 上保留原始 HTML ✅ 已实现（2026-09-30）

- `uparser-document-engine/src/model.rs:247` `Table` 增 `#[serde(default)] pub source_html: Option<String>`
  （原始、未重排的表格 HTML；结构化来源恒为 `None`）。
- `ascend.rs:261` 构造 `DocBlock::Table` 时把 `block.html` 原文填入。
- 验收：`document-json` 的表格对象新增该字段且旧 JSON 仍可反序列化（`serde(default)` 回归测试）。
- 工作量：约 60 行。风险：低（纯新增字段）。

### A.2 T-A2 · 渲染器引入表格策略，并把它做成 CLI 开关 ✅ 已实现（2026-09-30，默认值未翻转）

- `render/mod.rs` 新增 `pub enum TablePolicy { Gfm, PreferSourceHtml }`，
  `render_blocks` 的两条 `Block::Table` 分支改为三条：
  `PreferSourceHtml && source_html.is_some()` → 原文直出；`has_spans()` → 现有 `render_html_table`；
  其余 → 现有 `render_markdown_table`（行为不变）。
- `uparser-core` 侧 `RenderInput` 增 `table_policy`，CLI 增 `--table-format auto|gfm|source-html`。
  **与初稿的一处有意偏离**：`auto` **没有**按 spec 的 `DecodeKind` 自动切到
  `PreferSourceHtml`，而是等于今天的 `gfm`（`render::auto_table_policy`）。理由是各协议
  的榜单分数正是在现有表格渲染下测出来的，在没有 A.3 复测前翻转默认值，就是本方案
  自己在批评的"未度量的静默变更"。`source-html` 作为待评测的对照臂显式提供。
  另外确认了两个协议**原则上**不该直出：`generic-vlm` / `paddlex-structure` 的模型
  给的是 Markdown 管道表，`markdown_ir` 反向重建出 HTML——直出会用生成的标记替换
  模型真正写的内容，方向正好相反。
- **关键点**：A/B 必须是一个 flag，而不是一次代码分叉——否则无法在同一二进制上对每个协议分别取数。
- 验收：
  1. 单测 ✅：`document-engine` 两条（策略二选一、无 `source_html` 时回落到 grid）+
     `uparser-core` 两条（两臂输出不同、`auto` 仍是 grid）；
  2. 真实二进制 ✅：新增 CLI 集成测试 `source_html_table_arm_differs_from_the_default_grid_arm`
     （wiremock dots-ocr，`colspan` + `<br>` 的表格），实测 grid 臂丢掉 `<br>`（`a<br>b` → `ab`）
     而保留 `colspan`——**这与我最初的断言相反，是被真实输出纠正的**；
  3. 全量评测 ⛔ 见 A.3（环境阻塞）。
- 工作量：实际约 300 行（含 `Table` 新字段在 11 处字面量的机械补齐）。

### A.3 T-A3 · 全量对照评测 ⛔ 被环境阻塞（2026-09-30）

**当前不具备度量条件，已核实**：`127.0.0.1:19122` 现在服务的是 `Qwen3.8-27B`（不再是
`MinerU2.5-Pro`），`127.0.0.1:8011`（MonkeyOCRv2-B-Parsing）无响应；用 Qwen 跑
`dots-ocr` 协议实测整页空输出（模型不遵守该协议的 JSON 契约，D.3 的空白页警告当场报出来了）。
因此 mineru-vlm / monkeyocr-v2 / navidc-ocr / pipeline 四条 HTML 表格协议一条都跑不了。

待任一端点恢复后要做的（两臂同一二进制，只差一个 flag）：

- 每个协议跑 `--table-format gfm`（= 今天的 `auto`）与 `--table-format source-html`，
  OmniDocBench 全 1651 页 + ODL 全 200 篇；**native 也要跑**（见上方进度注）；
- 通过线：source-html 臂的 Table TEDS ≥ 现值，文本/顺序 Edit 退化 ≤ `0.002`；
  满足则把 `render::auto_table_policy` 翻转为按 `DecodeKind` 决定（该函数的文档注释里
  已写明这一步和理由），并更新 `UPARSER_LEADERBOARD.md`；
- 两臂数字都要留档，不得只留胜者。

顺带在同一轮里回答 monkeyocr-v2 的归属问题：把它的 `owns_document_assembly` 拆成两件事度量——
仅表格交回共享渲染器（其余仍自装配）vs 完全交回。

- 若"仅表格交回"后 Table Edit 不差于 `0.1040`、Text Edit 不差于 `0.0499`，则删除该协议在表格上的特例；
- 否则保留，但把判定迁到 T-C2 的 spec 字段（不再由 `monkeyocr_post` 反向被共享渲染器依赖）。
- 验收：结论写入 `UPARSER_LEADERBOARD.md`，两种形态的数字都留档（不得只留胜者）。
- 工作量：0 行代码 + 一轮评测（结论可能是"保持现状"，这也是有效产出）。

---

## B. 可观测性与协议成熟度（最低成本、最高即时收益）

> **进度（2026-09-30）**：B.1 / B.2 已实现并通过真实二进制验证，B.3 未开始。
> 实现记录与两处实测发现（native 的缓存命中仍要付 112ms 分析；模型协议 `model_ms` 占 99.5%）
> 见 `CLAUDE.md` 末节。

### B.1 T-B1 · `ParseResult.timing` 落地 ✅ 已实现（2026-09-30）

现状：`types.rs:368` 有字段，`runner.rs:567/828/1781/1899/1942` 全是 `Default::default()`。

- 在 `runner.rs` 的既有阶段边界打点（不新增阶段）：
  `detect`（`frontend` 检测）、`analyze`（`analyze_inner`）、`plan`（`preprocess_plan`）、
  `ingest`（栅格化/转换）、`model`（scheduler 或 `execute_native`）、
  `postprocess`、`assets`、`cache`、`total`。
- 键名定义为 `pub const TIMING_*: &str` 常量，避免又一组跨文件魔法字符串；单位固定毫秒 `f64`。
- 缓存命中路径（`runner.rs:437`）只填 `detect/analyze/total` 并保持 `cache_hit=true`，
  不得伪造 `model` 耗时。
- 渲染耗时属于 CLI 层，记在 `render` 键，由 `cli.rs`/`api.rs` 在 `render_*` 前后补。
- 验收：CLI 测试断言 `--format json` 的 `timing` 非空、`total >= 各阶段之和 - 1ms`、
  缓存命中运行的 `timing` 不含 `model` 键。
- 工作量：约 120 行。风险：低。

### B.2 T-B2 · `ProtocolSpec` 增加验证等级 ✅ 已实现（2026-09-30，`protocols` 已暴露；`doctor` 未接）

- `protocol_spec.rs:90` 增
  `pub validation: ValidationTier { VerifiedLive, OfflineOnly, SpeculativeContract }`
  与 `pub last_verified: Option<&'static str>`（形如 `"2026-09-20 OmniDocBench-1651"`）。
- 逐协议填真值（依据现有证据）：`native`/`mineru-vlm`/`monkeyocr-v2` → `VerifiedLive`；
  `navidc-ocr` → 按最近实跑填；`dots-ocr`/`generic-vlm`/`tesseract` → `OfflineOnly`；
  `paddleocr`/`paddlex-structure`/`pipeline` → `SpeculativeContract`。
- `cli.rs:1398 run_protocols` 输出这两个字段；显式选择 `SpeculativeContract` 协议时向 stderr
  写一行能力提示，并计入 `ParseResult.capability_notes`（该字段已存在，正好有了第一个真实来源）。
- 验收：`uparser protocols | jq '.[].validation'` 每项非空；穷尽 `match` 保证新增协议必须表态。
- 工作量：约 80 行。风险：极低。

### B.3 T-B3 · 给能力声明接上第一个真实消费者（否则删除）

`emitted_signals()` / `provides_reading_order()` 至今零消费者（非 adapter 侧唯一出现处是
`scheduler.rs` 的测试替身）。二选一，建议"接上"而不是删：

- `runner.rs` 在 postprocess 之前统一回填阅读顺序：
  `if !adapter.provides_reading_order() && page.blocks.iter().all(|b| b.reading_order.is_none())`
  → 调 `reading_order::assign_reading_order`；
- 同步删除 `pipeline_v2.rs:1524` / `paddleocr.rs` 内部各自的调用（去重复，同时让声明有意义）；
- `emitted_signals` 先只用于降级判定：`spans` 为 false 时跳过基于 spans 的合并（当前只有
  native 产出 spans），并在 `protocols` 中如实展示。
- 验收：新增"无序 blocks 经 runner 后有序"的测试；pipeline/paddleocr 的既有顺序测试不改断言仍绿。
- 工作量：约 100 行（含删除）。风险：中（改动已通过基准的 pipeline 路径，需 ODL 复跑一次）。

---

## C. 抽象泄漏：粒度、装配归属、领域逻辑外移

### C.1 T-C1 · `ProtocolAdapter` 引入执行粒度，消灭协议名特判

现状：`runner.rs:451` 与 `runner.rs:1560` 各有一处 `if protocol == "native"`；
`native.rs:1295` 的 `parse_page` 是一个"永远返回解释性错误"的假实现。

- `adapters/mod.rs:311` 增：
  ```rust
  pub enum Granularity { Page, Document }
  fn granularity(&self) -> Granularity { Granularity::Page }
  async fn parse_document(&self, doc: &DocumentInput, ctx: &ParseCtx)
      -> Result<DocumentOutcome, PageError> { /* 默认：走 scheduler 逐页 */ }
  ```
- `native` 覆写 `granularity() == Document` 与 `parse_document`，删除 `parse_page` 的假实现；
- `runner::execute_with_hooks` 用 `match adapter.granularity()` 取代字符串比较，
  `execute_native` 降级为 native adapter 内部的实现细节；
- `protocols` 输出 `granularity`（Agent 据此知道 `--window-size/--max-concurrency` 是否生效）。
- 验收：grep 全仓无 `protocol == "native"`；native 的现有 CLI/单测不改断言通过；
  ODL native 200 篇逐文件 diff 与冻结目录一致（这是 native 的既有 G-N 闸门）。
- 工作量：约 250 行。风险：中（触碰主干执行路径，但有逐文件 diff 闸门）。

### C.2 T-C2 · 装配归属迁到 spec，断开"共享渲染器 → 协议私有模块"的反向依赖

- `protocol_spec.rs` 增 `pub assembly: AssemblyOwner { Shared, Adapter }`；
- `render/mod.rs:69` 与 `runner.rs:650` 改为读 spec，不再调
  `monkeyocr_post::owns_document_assembly`（`monkeyocr_post.rs:556` 随之删除）；
- 自装配协议需在 adapter 上提供 `fn assemble_markdown(&self, &ParseResult) -> String`，
  `render_markdown` 通过 registry 取到它——渲染器从此不知道任何协议名。
- 验收：`grep -rn '"monkeyocr-v2"' src/render src/runner.rs` 无命中；
  monkeyocr-v2 的 Markdown 输出字节不变（现有测试足够，另加一条 golden）。
- 工作量：约 120 行。风险：低。

### C.3 T-C3 · 把领域语义从通用编排层抽成可选 pass

`runner.rs` 现有：`annotate_technical_standard_ir`(852)、`declared_mandatory_clause_ids`(893)、
`leading_normative_clause_id`(911)、`is_annex_heading`(932)、`reconcile_ocr_toc_with_native`(1110)、
`split_numbered_heading`(1257)、`likely_traditional_chinese`(1304) —— 国标/目录页语义写死在
所有文档都要经过的主干上。

- 新增 `semantic/passes/`，定义
  ```rust
  pub trait DocumentPass { fn name(&self) -> &'static str;
      fn applies(&self, profile: &DocumentProfile, hints: &ParseHints) -> bool;
      fn apply(&self, result: &mut ParseResult); }
  ```
- 把上述函数原样搬入 `TechnicalStandardPass` 与 `TocReconcilePass`（**只移动，不改逻辑**，
  连测试一起搬），在 `runner` 里按 `applies()` 顺序执行；
- CLI 增 `--no-semantic-passes`；每个生效的 pass 名写入 `capability_notes`（可自省）。
- 验收：移动前后同一文档的 JSON/Markdown 逐字节相同（这是本任务唯一的正确性标准）；
  `runner.rs` 行数下降 ≥ 400；`--no-semantic-passes` 下国标标注消失。
- 工作量：约 1 天（机械搬迁 + 一条字节相等测试）。风险：低但体量大。

---

## D. Pipeline：前提已作废——只剩"空白页有没有消失"一个问题

### D.0 现状（**2026-09-30 更正**）

本文初稿依据 `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md` v3（2026-08-27）与
`ARCHITECTURE_V2.0_EVALUATION_REPORT.md`（2026-08-25），写成"S1–S3 已实现但从未跑分，
当前分数仍是 ODL 0.8001 / Omni 75.78 / Table TEDS 27.53"。**这是过期结论**：
`UPARSER_LEADERBOARD.md`（数据截止 2026-09-21，pipeline 行 2026-09-14）已有 S1–S3 之后的
全量复测结果。

| 榜单指标 | 旧（2026-08-25） | 现（2026-09-14 全量） |
|---|---:|---:|
| ODL Overall | 0.8001 | **0.9086**（仅次于 mineru-vlm 0.9252） |
| ODL Table TEDS | 0.8364 | **0.9133** |
| Omni Text Edit ↓ | 0.1908 | **0.0706** |
| Omni Table TEDS | 0.7311 | **0.7868** |
| Omni Reading Order Edit ↓ | 0.2948 | **0.1530** |

因此 `pipeline` 已达 `VerifiedLive`（本轮已写入 `protocol_spec.rs`），原 D.2 的"降级为
experimental"分支不再触发——一个在两个公开榜单上都排第二的协议不该被移出默认构建。
这条误判本身就是 §E 的 T-E3 要解决的问题：同一事实散落在三份文档里，最旧的那份最容易
被当成现状。

**唯一仍然有效的缺口**：两轮复测都没有记录空白页数（旧版是 60 对 2），所以"空白页是否
随 S1–S3 消失"至今无证据。这正是 D.3 提供的信号。

### D.1 T-D1 · 缩窄为"补一次空白页计数"（工具已就绪）

- 用本轮落地的 D.3 能力重跑 `pipeline`（无需重跑评测器，只看 stderr / `warnings`）：
  `uparser parse --protocol pipeline --stats --fail-on-blank-pages 2 <doc>`；
- 通过线：ODL 200 篇 + Omni 1651 页中空白页 ≤ `2`（原始 MinerU 同模水平）。
- 工作量：0 行代码，半天机时（依赖 GPU + stage endpoint 起服务）。

### D.2 T-D2 · 仅在 D.1 不通过时执行

按 `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md` §1 的 D1/D4 继续修（表格 OCR 分流、
去重幽灵块），不再考虑 feature 降级。

### D.3 T-D3 · 空白页作为一等信号 ✅ 已实现（2026-09-30）

- 任一协议产出"全页无文本 block"时，写入 `ParseResult.warnings` 并在 `page_errors` 之外
  单独计数；`--fail-on-blank-pages <N>` 可让批处理及早失败。
- 验收：构造一个必然空白的页（mock adapter 返回空 blocks）→ warning 出现且计数正确。
- 工作量：约 60 行。风险：低。

---

## E. 横切：CI 特性矩阵与 lint 基线

| 任务 | 内容 | 验收 |
|---|---|---|
| T-E1 ✅ | CI 增加 `default` / `native` / `native,pdfium` 三列矩阵（`.github/workflows/ci.yml`），并在 test 步骤设 `NO_PROXY`（否则本地 wiremock 被代理拦截，失败看起来像协议 bug） | 本机三种配置全绿 |
| T-E2 ✅ | `clippy --workspace --all-targets -- -D warnings` 现在**真的通过**（此前 CI 里这行是失败的：vendored crate 有两处 `never_loop`，属 deny-by-default，整 crate 连检查都过不去）。vendored crate 用**逐条枚举**的 `#![allow(...)]`（不是 `clippy::all`，新问题仍会冒出来），两个自研 crate 零 warning；顺带修掉一处真实文档缺陷：`postprocess.rs` 里 `join_wrapped_lines` 的整段说明被挂到了 `merge_spans` 上 | 已验证；另跑一遍 `--features uparser-core/pdfium` 覆盖 pdfium 分支 |
| T-E3 | 文档收敛：57 份根目录 md 按"现状/计划/评测归档"分三目录，`CLAUDE.md` 只保留指针并修正已漂移的模块表 | 根目录 md ≤ 10；`ARCHITECTURE_FLOW.md` 与实际模块列表一致 |

---

## F. 排期与依赖

```
B.1 timing ─┐
B.2 spec    ─┼─ 无依赖，可立即并行（合计 ~1.5 天，零精度风险）
D.3 blank   ─┘

A.1 → A.2 → A.3        （~2 天代码 + 2~3 轮全量评测，收益最确定）
                 └─ C.2（装配归属，依赖 A.3 的结论）
C.1 granularity        （~1 天，依赖 B.2 的 spec 扩展位）
C.3 semantic passes    （~1 天，独立，可随时插入）
D.1 复测 → D.2 决策    （依赖 GPU 环境，与上面全部并行）
E.1/E.2/E.3            （~1 天，建议在 C 之前完成 E.1，否则 C 的改动测不全）
```

总计约 **8–11 人日**（不含 GPU 复测机时）。净代码：
`+ ~900 行`（策略/粒度/pass/timing）、`- ~600 行`（重复调用、假实现、特判），
若 D.2 走降级路线另有 ~9.5k 行移出默认构建。

## G. 明确不做

- 不在 A 完成前继续为单个协议加"自装配"豁免——那正是本轮要收敛的模式。
- 不合并 `types::Block` 与 `CanonicalDocument` 的存储表示；`--format json` 与
  `--format document-json` 的信息不相交是**数据模型决策**，应单独立项，
  不能塞进渲染/编排改造里顺手做。
- 不在 D.1 复测出数前对 pipeline 做任何新优化。
- 不为 `paddleocr`/`paddlex-structure` 补契约细节（无真实服务可对齐）。
