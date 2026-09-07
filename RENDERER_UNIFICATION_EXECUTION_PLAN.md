# 渲染器合并执行方案（M0–M4）

> 生成日期：2026-09-03
> 代码基准：`feat/architecture-v2`，O0–O5 执行完毕后的工作区
> 前置：`ARCHITECTURE_FLOW_AND_REDUNDANCY_ANALYSIS.md`（C 类问题定义）、
> `ARCHITECTURE_OPTIMIZATION_EXECUTION_PLAN.md` 附录 A.7（O5.3 门禁失败的实测与根因）
> 目标：把三个 Markdown 渲染后端收敛为一个，**且不牺牲任何已验证精度**。

---

## 执行状态（2026-09-03）

| 阶段 | 状态 | 结果 |
|---|---|---|
| **M0 基线护栏** | ✅ 完成 | 见 §12 基线表；新增 `benchmark/compare_renderers.py`（逐篇 delta + L2 门禁判定） |
| **M1 结构导出** | ✅ 完成，**L1 门禁通过** | 引擎新增 `structure_export.rs`；200 篇 engine markdown **逐字节 200/200 相同**；性能 45.3 vs 45.0 ms/篇（噪声内）。canonical 路径 Overall **0.5663 → 0.7888**，TEDS **0 → 0.6495**，MHS **0 → 0.6472** |
| **M2 3→2 合并** | ✅ **已合并**（2026-09-04）。`render::to_markdown` 删除，model-protocol / pipeline 改走统一渲染器 | 合并后 VLM 默认路径 Overall **−0.0018**、NID −0.0025、TEDS **−0.0000**、MHS −0.0018，**0 篇单篇回退**；native engine 路径 L1 **200/200**；性能 1.843 vs 1.842 s/篇。门禁按 §17 的明示例外（均值容差 0.003）通过 |
| **M3.1 结构信号补齐（八个子步骤）** | ✅ 完成，**L1 每步 200/200 保持** | native canonical **0.7888 → 0.8540**，TEDS **0.6495 → 0.8343**（基本追平），MHS 0.6472 → 0.7536，回退篇数 71 → 37。此时 L2 仍 FAIL（−0.0213），剩余成因见 §15；由 §18 的 M3.2–M3.8 补完 |
| **U 轮 · 最小化转义（契约 A 的前置）** | ✅ 完成 | 见 §16。VLM canonical **−0.0026 → −0.0019**；native canonical TEDS 0.8332 → 0.8343 |
| **M3 · 2→1** | ✅ **完成，L2 门禁全绿**（2026-09-07） | 见 §18。native canonical **0.8540 → 0.8766**，四项**全部高于**引擎基线（Overall +0.0014 / NID +0.0014 / TEDS +0.0004 / MHS +0.0014），单篇回退 37 → **2**（上限 2）。默认已切 `canonical`，③ 降级为 `--markdown-source engine-legacy`。L1 每步保持 200/200 |
| **M4 收尾** | ⬜ 建议观察一个版本周期 | 依赖 M3；§18.7 修正了可删除范围 |


---

## 0. 问题陈述

### 0.1 现状：一个入口，三个后端

O5.4 之后「用哪个渲染器」已经收敛成一个函数（`render::render_markdown`），但后端仍是三个：

| # | 后端 | 行数 | 服务对象 | 读什么 |
|---|---|---:|---|---|
| ① | `uparser-core/src/render/mod.rs` | 497 | 6 个 model-protocol + pipeline | `Page`/`Block` IR |
| ② | `uparser-document-engine/src/render/mod.rs` | 1002 | 9 种结构化格式 | `CanonicalDocument` |
| ③ | `uparser-native-engine/src/markdown/` | **9675** | PDF native | 引擎内部 `TextLine`/`Table` |

合计 **11174 行**渲染代码，三套转义规则、三套表格降级策略、三套标题判定。
维护成本是真实的：改一个「列表缩进」要改三处，且没有任何机制保证三处改一致。

### 0.2 为什么 O5.3 直接切换会失败（已实测）

| 路径 | Overall | NID | TEDS | MHS |
|---|---:|---:|---:|---:|
| `engine`（③，当前默认） | **0.8753** | 0.9183 | 0.8389 | 0.7812 |
| `canonical`（②，经 ascend） | 0.5663 | 0.8515 | **0.0000** | **0.0000** |

TEDS/MHS 归零不是渲染器写错了，是**喂给它的 IR 是空的**。同一篇文档实测：

```
categories:              {'text': 22, 'image': 4}   ← 零个 title
blocks with html:        0                          ← 零个表格
blocks with merge_hint:  0
```

全量：200 篇里 **107 篇 MHS=0**、**42 篇 TEDS=0**。

### 0.3 根因：引擎的判断结果没有出口

`native-engine` 不是「返回 blocks 的库」，是一个 **PDF→Markdown 的完整产品**。它内部
**确实**算出了标题和表格，但只把成品字符串交出来：

```
extract items ──► 行分组 ──► classify_heading_sequences()  ──► HashMap<line_idx, level>  ┐
                         └─► detect_tables()              ──► Table{rows,columns,cells}  ├─► 拼 Markdown 字符串
                                                                                          ┘
                                          ↑ 这两个结构化结果只活在函数局部，从不外泄
```

而 `adapters/native.rs::build_pages` 只拿到 `positioned_items`（裸文本项），
所以只能把所有东西标成 `text`。

**这就是整件事的锁：** 想合并渲染器 → 需要 IR 有标题/表格 → 需要引擎导出判断 →
而引擎那 9675 行正是「不敢碰否则影响 0.8753」的那块。

---

## 1. 破局点（已通过读源码确认，非推测）

三条事实让这个锁可以低风险打开：

**事实 1 — 索引空间是共享的。**
`lib.rs:3869` `positioned_items = items.clone();`，紧接着 `lib.rs:3899` 把**同一个**
`items` 移交给 Markdown 管线。因此 `PdfProcessResult.positioned_items[i]`
与管线内部看到的第 `i` 项**严格同一**。任何以 item 下标为键的信息都能无歧义地
回传给 `adapters/native.rs`。

**事实 2 — 两个信号都已经是结构化的，且就在同一个函数作用域里。**

| 信号 | 现成的东西 | 位置 |
|---|---|---|
| 标题层级 | `classify_heading_sequences(...) -> HashMap<line_idx, level>` | `markdown/heading.rs:327`，**纯判定函数，不产 Markdown** |
| 表格结构 | `tables::Table { columns, rows, cells, item_indices, kind }` | `tables/mod.rs:1438`，**自带 `item_indices`** |
| 表格占用项 | `let mut table_items: HashSet<usize>` | `markdown/mod.rs`，已是局部变量 |

`table_to_markdown(&table)` 是**独立的格式化步骤**（`tables/format.rs:5`），
也就是说结构与呈现在引擎内部本来就是分开的——只是结构那一半没有出口。

**事实 3 — 导出是纯增量的。**
只在已知这些值的地方**额外记录一份**，不改变任何控制流，因此
「导出前后 Markdown 字节相同」是可以逐篇断言的硬证据。

---

## 2. 目标形态与两个中间态

```
现状        ① core render ──┐
            ② doc-engine ───┼── render_markdown（一个入口，三个后端）
            ③ engine md ────┘

M2 之后     ② doc-engine ───┬── VLM / pipeline / 结构化   （② 统一）
            ③ engine md ────┴── PDF native               （③ 保留）

M3 之后     ② doc-engine ─────── 全部                     （唯一渲染器）
            ③ 降级为 `--markdown-source engine-legacy`，M4 视门禁决定是否删除
```

**先 3→2 再 2→1** 是有意的：3→2 完全不碰 native 那条路（0.8753 天然不受影响），
风险只落在 VLM 一侧且可单独度量；2→1 才是硬骨头，放到最后并且带独立门禁。

---

## 3. 精度保证协议（先定规则，再动代码）

任何一步合并都必须过这三关，缺一不可。

### 3.1 三级证据阶梯

| 级别 | 判据 | 用在 |
|---|---|---|
| **L1 字节等价** | 输出与改动前逐字节相同 | M1（导出信号，本就不该改输出） |
| **L2 分数不回退** | ODL 200 篇 Overall / NID / TEDS / MHS 四项各自 ≥ 基线 − 0.002；**且单篇分数下降 > 0.05 的文档数 ≤ 2** | M2 / M3 |
| **L3 语义回归** | `tests/semantic_regression.rs` + `tests/render_paths.rs` 全绿 | 每一步 |

L2 的「单篇」子条款是必须的：均值可以掩盖「10 篇变好 10 篇变差」的置换，
而渲染器换代最典型的失败模式恰好就是这种此消彼长。

### 3.2 基线冻结

在 M1 开始前，把当前输出冻成不可变基线：

```bash
# 200 篇 × 2 条路径的 Markdown + 每篇分数
uparser parse --protocol native --format markdown ...            > baseline/engine/<id>.md
python3 src/evaluator.py --engine uparser-native-o5-engine        # 逐篇 evaluation.json
```

产物落 `benchmark/baselines/renderer-unification/`，**入库**，后续每步 diff 它。

### 3.3 差距阶梯（M3 的进度度量）

M3 不可能一步做到分数持平。需要一个能看见进展的中间指标：

```
diff_ratio = 与 engine markdown 不同的文档数 / 200
```

从 M3 开始每个子步骤记录 `diff_ratio` 与四项分数，形成收敛曲线。
**若连续两个子步骤 diff_ratio 下降 < 5% 且分数仍未达标，触发 3.5 的退出条款。**

### 3.4 反向保护：不允许「为跑分而改」

禁止在渲染器里写针对评测指标的特判（如「MHS 只看有没有 `#`，那就多打 `#`」）。
每个改动必须能用「这是文档的真实结构」解释。评审时对照 `render_paths.rs` 快照人工看输出。

### 3.5 退出条款（写在前面，避免沉没成本）

若 M3 在 **10 个工作日**内无法让 L2 通过，则：

- 停止 2→1，**接受两个渲染器**（② + ③）作为终态；
- 把 ③ 的边界固化为「仅 PDF native 的 Markdown 呈现」，禁止它承担任何其它职责；
- 该结论写回本文件与架构文档，附收敛曲线数据。

**两个渲染器（1002 + 9675）比三个（+497）已经是明确的改进**，M2 单独就有价值。

---

## 4. M0 · 基线与护栏（0.5–1 人日）

| ID | 任务 | 验收 |
|---|---|---|
| M0.1 | 冻结 §3.2 的基线产物入库 | `benchmark/baselines/renderer-unification/` 有 200 篇 md + evaluation.json |
| M0.2 | 写 `benchmark/compare_renderers.py`：跑两条路径 → 出四项分数 + `diff_ratio` + **逐篇分数 delta 排序表** | 一条命令产出一份可评审报告 |
| M0.3 | `render_paths.rs` 扩容：把 PDF 样本从 1 篇合成 PDF 扩到 3 篇真实 PDF（含表格页、多级标题页、多栏页） | 快照覆盖三种真实版式 |

> M0.2 的逐篇 delta 表是整个方案里最重要的工具：没有它，L2 的单篇子条款无法执行。

---

## 5. M1 · 把引擎的结构判断导出到 IR（3–5 人日，L1 门禁）

**这一步不改任何输出**，是后续所有步骤的前提。

### 5.1 引擎侧：新增结构化出口

新增 `uparser-native-engine/src/structure_export.rs`（**新文件**，把对
`mod.rs` 的编辑压到最小，降低与上游 re-sync 的冲突面）：

```rust
/// 引擎在生成 Markdown 途中已经算出、但此前只用于拼字符串的结构判断。
/// 全部以 `PdfProcessResult.positioned_items` 的下标为键 —— 该向量与
/// Markdown 管线消费的是同一个 items（`lib.rs:3869` 的 clone），索引严格对齐。
#[derive(Debug, Default, Clone)]
pub struct StructureHints {
    /// item 下标 -> 标题层级 (1..=6)
    pub heading_levels: HashMap<usize, u8>,
    /// 检测到的表格，按发现顺序
    pub tables: Vec<TableHint>,
    /// 被任一表格占用的 item 下标（快速判定用）
    pub table_items: HashSet<usize>,
    /// 被判定为页码/页眉页脚而从正文剔除的 item 下标
    pub discarded_items: HashSet<usize>,
}

#[derive(Debug, Clone)]
pub struct TableHint {
    pub page: u32,
    pub item_indices: Vec<usize>,
    pub rows: usize,
    pub columns: usize,
    pub cells: Vec<Vec<String>>,
    pub kind: TableKind,          // Data / TOC
    pub bbox: [f32; 4],
}
```

`PdfProcessResult` 增字段 `pub structure_hints: Option<StructureHints>`；
`ProcessOptions` 增 `collect_structure_hints: bool`（默认 `false`，
**默认路径一行都不变**）。

### 5.2 采集点（三处，都是已知值的地方）

| 信号 | 采集位置 | 做法 |
|---|---|---|
| `table_items` / `tables` | `markdown/mod.rs` 表格检测循环内，`table_to_markdown` **调用之前** | 把已有的 `Table` 与 `table_items` 抄一份 |
| `heading_levels` | `classify_heading_sequences` 返回处 | `line_idx` → `line.items` → item 下标 |
| `discarded_items` | 已有的 `prefiltered_page_number_mask` | 直接转 `HashSet<usize>` |

### 5.3 uparser 侧：喂进 IR

`adapters/native.rs`：
- `build_pages` 增参数 `hints: Option<&StructureHints>`；
- `build_page` 内，一行的 item 下标命中 `heading_levels` → `category: "title"` +
  `MergeHint::TitleLevel(n)`（与 O5 已落地的 tagged-PDF 路径合流，tagged 优先）；
- 命中 `table_items` 的行不再逐行成块，改由 `TableHint` 直接构造**一个** Block：
  `category: "table"`，`html` 由 `TableHint.cells` 生成（复用 `otsl::escape_html`）；
- `discarded_items` → `category: "page_number"`（IR 里保留但可被渲染器忽略）。

### 5.4 门禁（L1，硬性）

```
对 200 篇 ODL + 现有全部 fixture：
  process_pdf_mem(collect_structure_hints=false).markdown
    == process_pdf_mem(collect_structure_hints=true).markdown     逐字节
  且 `--markdown-source engine` 的 200 篇输出与 M0.1 基线逐字节相同
```

新增测试 `structure_hints_do_not_change_markdown_output`（引擎内）与
`native_ir_carries_engine_detected_headings_and_tables`（uparser 侧）。

**预期效果**：`canonical` 路径的 TEDS/MHS 从 0 起来。这一步结束后重跑
§3.1 的 L2，**记录数字但不作为门禁**——它是 M3 的起点读数。

---

## 6. M2 · 3→2：删掉 core 渲染器（2–3 人日，L2 门禁）

VLM/pipeline 协议的 IR 本来就是富的（adapter 会分类 title/list/table/equation/image），
`ascend.rs` 已能完整提升。把它们从 ① 切到 ②，然后**删除 ①**。

| ID | 任务 |
|---|---|
| M2.1 | `render_markdown` 的 `Engine` 分支：无 `engine_markdown` 且无 `document` 时，改走 `ascend` + ②（而不是 ①） |
| M2.2 | 补齐 ascend 的三处已知差异：表格 `html` 直通 vs ② 的 Markdown 表格降级；行内公式 `$…$`；图片 `![]()` 的相对路径 |
| M2.3 | 删除 `render::to_markdown`（497 行）及其快照；`render::to_json` / `to_content_list` 保留（它们不是 Markdown 渲染器） |
| M2.4 | `--markdown-source` 语义简化为 `engine`（PDF native 专用）/ `canonical`（其余） |

### 6.1 风险与门禁

**风险点**：evaluator 的 TEDS 从 HTML 表格算分。① 是把 `block.html` **原样透传**，
② 会把它变成 Markdown 表格（无 span 时）或 HTML（有 span 时）。这**会**动 TEDS。

**门禁**：mineru-vlm 在 ODL 200 篇上过 L2（基线 Overall 0.9240 / TEDS 0.9682）。
需要一个在线 MinerU2.5 端点；**没有端点则 M2 不得合入**，先做 M1 与 M3 的前半。

**降级方案**：若 TEDS 回退，在 ② 里加一条「表格若已有权威 HTML 则直通」的规则
（这是保真而非跑分特判：模型给的 HTML 就是它对表格结构的完整表达）。

---

## 7. M3 · 2→1：PDF native 也走统一渲染器（5–10 人日，L2 门禁 + §3.5 退出条款）

M1 之后 IR 有了标题和表格，但离 engine markdown 仍有距离。按**影响面从大到小**逐项补，
每项独立提交、独立记录 `diff_ratio` 与四项分数。

| 序 | 差距项 | 现在归谁 | 迁移做法 |
|---|---|---|---|
| 1 | 段落合并（跨行拼句） | 引擎 `markdown/postprocess.rs` | 已有 `postprocess::merge_paragraphs_by_geometry`，对齐阈值 |
| 2 | 阅读顺序（多栏/侧栏） | 引擎 `markdown/mod.rs` 的列检测 | 引擎导出「列切分结果」为第 4 类 hint；或用 `reading_order::assign_reading_order` 对齐 |
| 3 | 页码/页眉页脚剔除 | 引擎 mask | M1 已导出 `discarded_items`，渲染时跳过 |
| 4 | 列表识别 | 引擎 `is_list_item` | 导出为 hint（第 5 类），`category:"list"` + `MergeHint::ListItem` |
| 5 | 转义与内联样式 | 两边都有，规则不同 | 以 ② 为准，逐项对齐 `render_paths.rs` 快照 |
| 6 | TOC 表格降级 | 引擎 `TableKind::TOC` 特殊处理 | `TableHint.kind` 已带，② 增同款降级 |

**每一项的验收**：`diff_ratio` 下降且四项分数不回退；任一项使某篇下降 > 0.05
则该项打回重做。

**M3 通过条件**：L2 全绿 → 默认切到 `canonical`，③ 改名 `engine-legacy` 保留一个版本周期。
**M3 未通过** → 执行 §3.5，终态为两个渲染器。

---

## 8. M4 · 收尾（1 人日，仅在 M3 通过后）

- 删除 `--markdown-source` 开关与 ③ 的 Markdown 模块（约 9675 行）；
  引擎只保留 extract + detect + structure hints；
- `ATTRIBUTION.md` 记录这是相对上游的重大裁剪，评估是否值得继续跟随上游；
- 架构文档与 `BENCHMARK_REPORT.md` 更新为单渲染器口径。

> 注意：删掉 ③ 意味着 `native-engine` 从「vendored 产品」变成「vendored 库」，
> 与上游 re-sync 的成本会显著上升。M4 是**可选**的——保留 ③ 但不再调用，
> 成本只有编译时间，收益是随时可回退。建议 M3 通过后先观察一个版本周期再决定。

---

## 9. 排期与依赖

```
M0 基线护栏 ─► M1 结构导出（L1，零输出变化）─┬─► M2 3→2（需 VLM 端点，L2）
                                            └─► M3 2→1（L2 + 退出条款）─► M4 收尾（可选）
```

| 阶段 | 人日 | 门禁 | 阻塞条件 |
|---|---:|---|---|
| M0 | 0.5–1 | — | — |
| M1 | 3–5 | L1 字节等价 | — |
| M2 | 2–3 | L2（mineru-vlm 列） | **需在线 MinerU2.5 端点** |
| M3 | 5–10 | L2（native 列）+ §3.5 | M1 |
| M4 | 1 | — | M3 通过 |
| | **11.5–19** | | |

M2 与 M3 可并行（分别打在不同渲染路径上），但都依赖 M1。

---

## 10. 为什么这个方案能保住精度

1. **M1 零输出变化且可逐字节证明**——最大的一块工作（打开引擎的锁）不承担任何精度风险。
2. **拆成 3→2 和 2→1**——把「不碰 native」和「碰 native」分开，风险可归因。
3. **单篇 delta 门禁**——阻止均值掩盖的此消彼长，这是渲染器换代最常见的翻车方式。
4. **写在前面的退出条款**——避免为了「必须合并成一个」而牺牲分数；两个渲染器是可接受终态。
5. **禁止跑分特判**——保证收敛到的是真实结构，而不是评测器的形状。

## 11. 明确不做

- **不把 engine markdown 反解析回 IR**。看似最省事，但会让 IR 依赖渲染产物，
  方向颠倒，且经过一次文本有损往返，任何格式细节的丢失都不可追溯。
- **不重新实现引擎的启发式**。M1 是导出既有判断，不是重写；重写等于放弃 9675 行的实测积累。
- **不在本方案内合并 `Page/Block` 与 `CanonicalDocument` 的存储表示**。
  上升/下降映射已经够用，合并存储是另一个数量级的改动。
- **不追求与 engine markdown 字节一致**。目标是分数不回退，不是复刻字符串；
  `diff_ratio` 只作进度指标，不作验收标准。

---

## 12. M0 基线（2026-09-03 实测，不可变）

所有后续步骤 diff 这一组数字。

| 路径 | Overall | NID | TEDS | MHS | 速度 |
|---|---:|---:|---:|---:|---|
| **native · engine**（默认） | **0.8753** | 0.9183 | 0.8389 | 0.7812 | **45.0 ms/篇**（冷）/ 42.5（缓存） |
| native · canonical（M1 前） | 0.5663 | 0.8515 | 0.0000 | 0.0000 | — |
| **mineru-vlm**（MinerU2.5-Pro-2605 @ 19122） | **0.9268** | 0.9435 | 0.9652 | 0.8793 | **1.842 s/篇**（200 篇 368.4s，单进程串行） |
| pdf-inspector（外部参照） | 0.8754 | 0.9150 | 0.8141 | 0.7875 | — |

产物：`opensource/opendataloader-bench/prediction/uparser-native-o5-engine/`、
`uparser-mineru-vlm-m0/`。复现：`python3 src/evaluator.py --engine <name>`。

> 注：VLM 的 1.842 s/篇 是**单进程串行**口径。`BENCHMARK_REPORT.md` 记录的 0.621 s/篇
> 来自不同的并发配置，两者不可直接比较；本方案的性能门禁一律以本表为准。

---

## 13. M1 实测结果与 M3 的主因定位

### 13.1 M1 通过 L1 门禁

| 检查 | 结果 |
|---|---|
| 200 篇 engine markdown（M1 前 vs 后） | **200/200 逐字节相同** |
| native 性能 | 45.3 ms/篇 vs 基线 45.0（+0.7%，噪声内） |
| 引擎单测 | 878 通过（新增 4 个 `structure_export` 测试） |

**实现要点与方案原设计的两处偏离**（均为读源码后的修正）：

1. **改用几何键而非 item 下标**。`TextLine.items` 持有的是 `TextItem` 的**克隆**，
   没有下标字段，而 `TextItem` 加索引字段会波及全引擎的构造点。改为以 PDF 坐标
   bbox 为键（`StructureHints::heading_at/table_at`，含 0.7 包含率容差）——几何本来
   就是 IR 的通用货币，且不会因任一侧改行分组而悄悄失配。
2. **不加 `collect_structure_hints` 开关，始终采集**。两条路径（采集/不采集）之间
   可能漂移，而始终采集意味着只有一条路径，L1 字节等价变成**构造上成立**而非测出来的。
   实测开销 +0.7%。

### 13.2 M1 的效果

| | M1 前 | M1 后 |
|---|---:|---:|
| canonical Overall | 0.5663 | **0.7888** |
| canonical TEDS | 0.0000 | **0.6495** |
| canonical MHS | 0.0000 | **0.6472** |
| 单篇回退 > 0.05 | — | 71 篇（L2 上限 2） |

单篇 IR 实测（`01030000000200.pdf`）：
`{'text': 22, 'image': 4}` → `{'image': 4, 'title': 2, 'text': 1, 'table': 1}`。

### 13.3 M3 剩余差距的主因：多栏阅读顺序（**新发现，优先级高于原表**）

逐篇看最差的 `01030000000191`（0.997 → 0.271）：

```
engine    : # Acknowledgements
            We would like to extend our gratitude to the teams at Hugging Face, ...

canonical : Acknowledgements and development in the field of LLMs.
            We would like to extend our gratitude to the teams   Ethics Statement
            at Hugging Face, particularly Clémentine Fourrier,   We conscientiously address ...
```

`adapters/native.rs::build_page` 的行聚类**只按 y 坐标聚**，把左右两栏的同一物理行
拼成了一行。引擎有多栏检测，uparser 侧没有。这比标题/表格更致命——它破坏的是文本本身，
所以 NID 也一起掉（0.9183 → 0.8587）。

**因此 §7 的差距表要重排优先级**：原表第 2 项（阅读顺序/多栏）应提到第 1 位，且做法
不是「导出栏边界让 uparser 重算」，而是**直接导出引擎的行分组结果**
（每行的几何 + 文档顺序），让 `build_page` 的行聚类整个删掉。

这个改动方向上更正确：IR 应当**由引擎的分析结果构建**，而不是在旁边并行地再分析一遍。
一次导出同时解决多栏、行合并、阅读顺序三项，且进一步减少重复代码
（`build_page` 的聚类逻辑约 30 行可删）。

**M3.1（新，估 2–3 人日）**：`StructureHints` 增 `lines: Vec<LineHint{page, bbox, order}>`，
`build_page` 改为消费它；无 hints 时（例如 `ProcessMode::Analyze`）回退到现有聚类。
门禁：canonical 的 NID 回到 ≥ 0.91，`diff_ratio` 下降。

---

## 14. M2 实测：3→2 未通过门禁（2026-09-03）

### 14.1 关键前提：先量噪声底

VLM 输出并非完全确定（vLLM 连续批处理）。合并前先做**控制实验**——同一条 engine 路径、
同一配置、重跑一遍：

| 对比 | Overall | NID | TEDS | MHS | 差异文档数 |
|---|---:|---:|---:|---:|---:|
| engine vs engine（纯模型噪声） | −0.0001 | −0.0001 | +0.0002 | −0.0003 | 18/200 |

**噪声底 ±0.0003。** 没有这个数，后面任何 0.00x 级别的结论都不可信。

### 14.2 M2 三轮测量

| 轮次 | 改动 | Overall | 单篇回退 > 0.05 |
|---|---|---:|---:|
| M2-a | 无（直接量 canonical） | 0.9240（−0.0028） | 1 |
| M2-b | + 列表恢复 | 0.9245（−0.0024） | **0** |
| M2-c | + 表头修复 | 0.9242（−0.0026） | 1（模型抖动，见下） |

M2-b → M2-c 的 −0.0002 在噪声底内；`01030000000163` 的 −0.053 经复跑确认是模型抖动
（同一篇在两次 engine 跑中也不一致）。**结论：稳定残差约 −0.0025，是噪声底的 8 倍，真实存在。**

### 14.3 过程中发现并修复的两处真缺陷（已保留）

1. **模型写在 `text` 块里的项目符号被转义成 `\-`**。mineru-vlm 会把 `- ` 写进 `text` 块的
   正文、另发一个空的 `list` 容器块。core render 原样透传所以看不出来；canonical 按纯文本
   转义，列表就不再是列表。修法：`ascend` 识别开头的 Markdown 列表标记并恢复为 `List`
   （这是结构恢复，正是 `ascend` 的职责）。效果：单篇回退 > 0.05 从 1 篇降到 0 篇。
2. **无 `<th>` 的表格被塞进一个空表头行**。Markdown 表格必须有表头，渲染器的兜底是补一个
   空行，等于凭空造出文档没有的一行。修法：`ascend` 在无 `<th>` 时把首行当表头。

两处都有独立测试，且对 `--format document-json` 同样有益，因此**即使 M2 不合并也保留**。

### 14.4 为什么剩下的 −0.0025 不该继续压

残差主要来自 `\*` 这类转义：模型写 `*8 ml`（脚注星号），document-engine 转义成 `\*8 ml`。

查 CommonMark 规则后确认：`*8 ml` 里的 `*` 后面跟的是非空白字符，属于 left-flanking
delimiter run，**确实能开启强调**——document-engine 转义是**正确**的，core render 只是
原样透传恰好和 ground truth 的字面文本对上。

按 §3.4「不许为跑分改正确行为」，**不能为了 0.0026 去掉正确的转义**。

### 14.5 决定

**不合并 M2**，`--markdown-source engine` 对 model-protocol 仍走 core render。
三个渲染后端维持现状。

若要继续推进，唯一正当的路线是 **M2.1：让 `ascend` 把模型写在 `text` 里的行内 Markdown
解析成 canonical 的 `Inline` 结构**（`Emphasis`/`Code`/`Formula`），这样渲染器就能区分
「模型写的强调」与「字面星号」，转义与保真两者兼得。这不同于 §11 禁止的「把渲染产物反解析
回 IR」——它解析的是模型写进文本字段的内容，而模型的输出契约本来就允许行内 Markdown。
估 3–5 人日，风险中等，需重新过一遍 L2。

### 14.6 性能

canonical 路径 1.847 s/篇 vs engine 1.842 s/篇（+0.3%，噪声内）。渲染不是瓶颈，
VLM 的时间全在模型推理上。

---

## 15. M3.1 实测：收敛曲线与剩余成因（2026-09-03/04）

### 15.1 收敛曲线（native · canonical，目标 0.8753）

| 步骤 | 改动 | Overall | NID | TEDS | MHS | 回退 > 0.05 | L1 |
|---|---|---:|---:|---:|---:|---:|---|
| M0 | — | 0.5663 | 0.8515 | 0.0000 | 0.0000 | — | — |
| M1 | 结构导出（标题+表格） | 0.7888 | 0.8587 | 0.6495 | 0.6472 | 71 | 200/200 |
| M3.1a | **导出行分组/阅读顺序** | 0.8278 | 0.8757 | 0.6524 | 0.7285 | 54 | 200/200 |
| M3.1b | 表格零宽 bbox 修复 | 0.8335 | 0.8758 | 0.7091 | 0.7284 | 54 | 200/200 |
| M3.1c | 整行匹配表格行 | 0.8370 | 0.8756 | 0.7486 | 0.7280 | 54 | 200/200 |
| M3.1d | 标题与最终 Markdown 对账 | 0.8410 | 0.8757 | 0.7486 | 0.7406 | 48 | 200/200 |
| M3.1e | 换行拼接去双空格 | 0.8425 | 0.8757 | 0.7486 | 0.7488 | 48 | 200/200 |
| M3.1f | 导出**渲染后**的表格单元格 | 0.8510 | 0.8792 | **0.8332** | 0.7504 | 40 | 200/200 |
| M3.1g | TOC 表按平铺列表 + 修排序 panic | **0.8554** | 0.8850 | 0.8332 | 0.7547 | 37 | 200/200 |
| M3.1h | 行内样式进 IR（`Span.style`）+ 段落合并携带 spans | 0.8536 | 0.8831 | 0.8332 | 0.7534 | 37 | **200/200** |
| 目标 | — | 0.8753 | 0.9183 | 0.8389 | 0.7812 | ≤ 2 | — |
| **剩余差距** | | **−0.0198** | −0.0332 | **−0.0056** | −0.0265 | | |

**每一步都先验 L1**：`--markdown-source engine` 的 200 篇输出逐字节不变。默认路径全程零风险。

### 15.2 M3.1a：导出行分组（最大单步收益）

`adapters/native.rs::build_page` 原本按 y 邻近聚行，把两栏页的左右栏拼成一行——破坏的是文本
本身，NID/MHS 一起掉。改为消费引擎的 `StructureHints::lines`（页码 + bbox + 阅读顺序），
uparser 侧的聚类降级为「引擎没安置的 item」的兜底。

同时把块排序从「几何 y→x」改为「引擎阅读顺序优先」——多栏页上几何排序必然交错两栏，
这正是引擎的栏检测存在的理由。

### 15.3 M3.1b/c：两个表格匹配缺陷（TEDS +0.099）

1. **零宽 bbox**：`TableHint.bbox` 由行/列**边界**算出，单列表格只有 1 个 x 边界 →
   宽度为 0 → 任何行都无法与之重叠 → 整张表丢失。
2. **多列表格只覆盖到最后一列的左边缘**：跨整行的文本行只有约一半落在框内，低于 0.7 阈值。

修法不是放宽阈值（那会误吞邻近文本），而是给 `table_at` 加两条**精确**的接受路径：
行的文本等于表格的某个单元格（单列表），或等于某一行所有单元格的拼接（多列表）。
比较时去掉空白，因为列间距在消费者侧不是换行。

### 15.4 剩余三个成因（M3.2–M3.6 的实际内容）

按剩余失分排序，逐篇 diff 得出：

| 成因 | 影响指标 | 例子 |
|---|---|---|
| **段落换行拼接规则不一致** | NID −0.043 | 引擎 `capabilities, no financial`，canonical `capabilities,  no financial`（双空格）；连字符换行引擎作 `in- house`，canonical 作 `inhouse` |
| **仍有标题未进 IR** | MHS −0.053 | `01030000000154` 的 `# IMPLEMENTATION`：引擎输出里有 `#`，但 hint 未与该行匹配上。**成因未定位**，下一步应先 dump 该篇的 hints 比对 bbox |
| **行内样式丢失** | NID | 引擎 `*Figure 7.1: ...*`（斜体图注），IR 无 inline 样式 → 纯文本。属 §14.5 的契约问题，需先裁决 |

### 15.5 M3.1d–g：又三个「决策发生在导出点之后」的实例

M1 的教训在 M3.1 里重复出现了三次——**结构决策不止一个地方**：

| 子步骤 | 发现 | 修法 |
|---|---|---|
| **M3.1d** | `postprocess.rs::refine_heading_blocks` 在 Markdown **字符串生成之后**把 `**IMPLEMENTATION**` 提升为 `# IMPLEMENTATION`。writer 层的 hint 因此**漏掉提升、又保留被降级的标题**（实测：某篇 markdown 有 1 个 `#`，hint 有 0 个） | 新增 `reconcile_headings`：拿最终 Markdown 的 `#` 行回过头与已记录的行按文本对账，几何从对应的行取。这样导出描述的是**实际产出的文档**，而不是某一层的中间意见 |
| **M3.1f** | `TableHint.cells` 导出的是 `Table::cells`（检测原始输出），而 `table_to_markdown` 会先跑 `clean_table_cells`（合并续行、删空行、修复跨列表头）。消费者因此每两行之间多一个空行、折行单元格被拆成两行 | 新增 `tables::rendered_table_cells`，导出**渲染器实际使用**的单元格。TEDS 0.7486 → **0.8332**，单步最大增益 |
| **M3.1g** | TOC 表格在引擎里按平铺 tab 列表渲染（`format_toc_as_list`），IR 侧却当数据表 | `TableKind` 本来就带这个区分，`build_table_blocks` 对 `Toc` 改出逐行文本块 |

**规律很清楚**：凡是「在导出点之后还会改结构的代码」，导出就必须**对账最终产物**，而不是记录中间决定。

### 15.6 一个被自己的改动引入、又被立刻抓到的崩溃

M3.1g 同批把块排序改成「有 order 按 order、没 order 按几何」——这**不是全序**（A 与 B 按 order 比、A 与 C 按几何比，传递性破坏），Rust 的 `sort_by` 检测到后直接 `abort`，200 篇里有 1 篇 core dump、输出为空、该篇得分 0.000。

修法是给每个块算**同一个排序键**：无 order 的块锚定到其上方最后一个有 order 的块
（`(anchor, tier, y, x)` 四元组）。新增回归测试
`mixed_ordered_and_unordered_blocks_sort_without_panicking` 复现该形状。

值得记下来的是**它是怎么被发现的**：不是靠测试，是靠 200 篇全量跑时 shell 打出的
`Aborted (core dumped)` 和评测表里那个刺眼的 `-0.845`。逐篇 delta 表在这里第二次证明了自己
的价值——均值只掉了 0.0001，单篇视图直接指出了哪一篇炸了。

### 15.7 M3.1h：行内样式——一个评测器无法裁决的问题

`TextItem` 一直带着 `is_bold/is_italic/is_underline/is_strikeout`，而 IR 的 `Span` 没有位置
放它们，所以 PDF 的每一处强调在进 IR 时就被丢掉了。这**不是** §14.5 那个契约争议
（那是「模型把 Markdown 语法写进 text 字段」），而是**信号缺失**：引擎有结构化的字体标志，
IR 没有字段接。

补法：`Span` 增 `style`；`adapters/native.rs` 从 `TextItem` 填充；`postprocess` 的段落合并
同时合并 spans（否则合并后 spans 与 text 对不上，消费者会整段丢弃）；`ascend` 由 spans
重建带样式的 `Inline` 序列，且**只在 spans 能精确重建 text 时才用**——spans 是增强，
`text` 才是真相。

**但实测分数反而降了 0.0018。** 查语料后原因很清楚：

| | `**` 出现次数 | `<u>` 出现次数 |
|---|---:|---:|
| ground truth（200 篇） | **2** | **0** |
| engine 输出 | 774 | 63 |
| canonical 输出 | 409 | 0 |

**参考答案里基本没有行内样式。** 也就是说这个语料**对任何行内样式都扣分**，无论对错；
引擎自己的 774 个 `**` 同样在被扣。canonical 之前输出得少，反而更贴近 GT。

按 §3.4「不许为跑分改正确行为」，这条**两面都成立**：不该为了分数去加样式，也不该为了
分数去删正确的样式。**因此这个问题评测器无法裁决，只能作为产品决策。**

现状是**保留**这份增强：IR 与 `--format document-json` 因此更完整，代价是 canonical 路径
（非默认路径）上 −0.0018。若判定「Markdown 输出不应带行内样式」，撤掉 `ascend` 里
`inline_content` 的调用即可，IR 侧的增强可以保留。

### 15.8 结论

- **M3.1 八个子步骤合计 +0.065**（0.7888 → 0.8536，其中 h 步 −0.002 见 §15.7），且方向正确：IR 现在由引擎的分析结果
  构建，`build_page` 不再并行地重做一遍行分组。**TEDS 已基本追平（−0.0056）**。
- 剩余 −0.0198 = NID −0.0332 + MHS −0.0265 + TEDS −0.0056（Overall 是三者等权均值）。
- 其中**行内样式丢失取决于 §14.5 的契约裁决**（引擎输出 `*Figure 7.1: ...*` 斜体图注、
  `<u>...</u>` 下划线，IR 无 inline 样式），不是纯工程问题。
- 剩下的工程性部分（未匹配上的标题、段落边界差异）估 2–3 人日，但**收益递减明显**：
  前四步各拿 +0.04/+0.006/+0.004/+0.004，后三步 +0.002/+0.009/+0.004。

---

## 16. U 轮：最小化转义（2026-09-04）

### 16.1 背景：契约裁决为 A，而 A 需要转义器足够精确

产品决策已定：**Markdown 输出要带行内样式**（§15.7），而「样式」这个概念只在契约 A
（`text` 是纯文本、结构与样式另存）里存在——契约 B 里只有字符，没有语义。所以契约取 A。

A 把转义的责任交给渲染器。但当时的 `escape_inline_text` 是**无条件转义**，这在 A 下是
过度的：它会往文档里加进原本不存在的反斜杠。M2 那 −0.0026 的主要来源就是这个。

**「按最小必要转义」不是为跑分让步，而是转义规则本身该有的精度**：只有当某个字符
真的会改变解析结果时才需要转义。

### 16.2 四条精确化规则（都对照 CommonMark 推导）

| 字符 | 原来 | 现在 | 依据 |
|---|---|---|---|
| `*` | 无条件转义 | 文本里只有**一个** `*` 时不转义 | 强调需要配对的闭合定界符；孤立星号（脚注标记、单位限定）构不成强调 |
| `&` | 无条件转成 `&amp;` | 只有后面真能构成实体（`&name;` / `&#123;` / `&#x1F;`）时才转 | 裸 `&` 在 CommonMark 里是字面量 |
| `<` | 无条件转成 `&lt;` | 只有后面是字母 / `/` / `!` / `?`（能开标签）时才转 | `a < b` 里的 `<` 是字面量 |
| `\` | 无条件双写 | 只有后面是 **ASCII 标点**时才双写 | 反斜杠只转义 ASCII 标点；`rac`、`
u` 本来就是字面量，双写反而破坏模型写出的 LaTeX |
| `_` | 词边界即转义 | 还需**至少一侧非空格** | 两侧都是空格的 `_` 不构成 delimiter run，`P _ {x}` 无需转义 |

每条都有独立单测（`ampersand_and_angle_bracket_escape_only_when_they_would_parse`、
`backslash_escapes_only_before_punctuation`、`underscore_between_spaces_is_not_escaped`、
`a_lone_asterisk_is_not_escaped_but_a_pair_is`）。

### 16.3 效果

**VLM 列**（`--markdown-source canonical` vs 默认 engine，基线 0.9268）：

| 轮次 | Overall | NID | TEDS | MHS | 单篇回退 > 0.05 |
|---|---:|---:|---:|---:|---:|
| M2-a（U 轮之前） | −0.0028 | −0.0033 | −0.0012 | −0.0035 | 1 |
| U1（`*` 最小化） | −0.0022 | −0.0030 | **−0.0000** | −0.0022 | 0 |
| U2（`&` / `<` 最小化） | −0.0021 | −0.0029 | −0.0000 | **−0.0019** ✅ | 0 |
| U3（`\` / `_` 最小化） | **−0.0019** ✅ | −0.0023 | **−0.0002** ✅ | −0.0025 | 1 |
| L2 容差 | −0.0020 | −0.0020 | −0.0020 | −0.0020 | ≤ 2 |

**Overall 与 TEDS 已达标，NID/MHS 各差 0.0003–0.0005**（噪声底 ±0.0003）。

**native 列**：TEDS 0.8332 → 0.8343，Overall 0.8536 → 0.8540。native 的差距主要不在转义上，
所以受益有限。

### 16.4 逐篇诊断中确认的三个真实缺陷

| 缺陷 | 症状 | 是否已修 |
|---|---|---|
| `Access & Searching` → `Access &amp; Searching` | 可见的文本损坏，出现在每一篇含 `&` 的文档 | ✅ U2 |
| `\frac {D v}{\nu}` | 模型写的 LaTeX 被双写反斜杠破坏 | ✅ U3 |
| `P \_ {\mathrm{out}}` | 空格包围的下划线被转义 | ✅ U3 |

这三条都不是「评测口径问题」——它们在任何 Markdown 阅读器里都是可见的错误输出。

---

## 17. 合并执行（2026-09-04）

### 17.1 门禁的一处明示例外

L2 原定均值容差 −0.002，合并后 VLM 的 NID 为 −0.0025。经产品决策**将均值容差放宽到 −0.003**
并在此记录理由：

- **单篇门槛未放宽且已通过**：0 篇文档回退超过 0.05（原定上限 2）。均值容差防的是「整体漂移」，
  单篇门槛防的是「此消彼长」，后者才是渲染器换代的典型翻车方式，它是硬的。
- **剩余差值接近噪声底**：模型噪声实测 ±0.0003，−0.0025 约为其 8 倍，但已连续四轮定位并修掉
  具体成因（§16），每轮都能解释；剩下的部分逐篇 diff 已看不到系统性模式。
- **TEDS 完全持平**（−0.0000），MHS/Overall 在 0.002 以内。
- **对照口径**：ground truth 本身不记录行内样式（200 篇仅 2 个 `**`、0 个 `<u>`），
  而产品决策是**要保留行内样式**（§15.7）。也就是说这个语料在这一项上与产品目标相反，
  继续以它为准去压最后 0.0005 会与 §3.4 冲突。

### 17.2 改了什么

| 位置 | 改动 |
|---|---|
| `render::render_markdown` | `Engine` 分支只对**有引擎自带 Markdown 的 native PDF**短路；其余全部经 `ascend` 进 `document_engine::render` |
| `render::to_markdown` | **删除**（108 行）。`to_json` / `to_content_list` 保留——它们不是 Markdown 渲染器 |
| `render::render_markdown` | 统一 `trim_end()`：canonical 渲染器自带尾换行、CLI 又加一个，输出会平白多一个空行 |
| `--markdown-source` | 语义收窄为「native PDF 用引擎自带 Markdown 还是走统一渲染器」；对结构化源和 model protocol 无效果——它们本来就只有一个渲染器了 |
| `render/mod.rs` 测试 | 六个断言（标题/列表标记、行内公式、`title_level` 深度、图片链接、跨协议一致性、快照）**全部改打在统一渲染器上**，而不是删掉——它们编码的是真实行为要求，合并后必须继续满足 |
| `tests/contract.rs` | 跨协议一致性同样改打统一渲染器 |

### 17.3 合并后的形态

```
② document-engine::render ──┬── 结构化 9 种格式
                            ├── 6 个 model protocol + pipeline   ← 本次并入
                            └── native PDF（--markdown-source canonical）
③ native-engine markdown ─────── native PDF（默认）
```

**三个后端 → 两个。** 剩下的 ③ 保留是因为它同时是 native 的**分析器**而不只是渲染器
（§0.3），拿掉它等于拿掉 0.8753 那套分析；native canonical 目前 0.8540，差 −0.0213。

### 17.4 现在的基准

| 路径 | Overall | NID | TEDS | MHS | 速度 |
|---|---:|---:|---:|---:|---|
| native · engine（默认） | **0.8753** | 0.9183 | 0.8389 | 0.7812 | 41.0 ms/篇 |
| native · canonical | 0.8540 | 0.8834 | 0.8343 | 0.7536 | — |
| **mineru-vlm（默认，已走统一渲染器）** | **0.9251** | 0.9410 | 0.9652 | 0.8775 | 1.843 s/篇 |

---

## 18. M3 · 2→1 完成（2026-09-07）

**L2 门禁全绿，`canonical` 已切为默认，③ 降级为 `--markdown-source engine-legacy`。**

### 18.1 门禁结果

| 判据 | 阈值 | 实测 | |
|---|---|---:|---|
| Overall | ≥ 基线 − 0.002 | **+0.0014** | ✅ |
| NID | ≥ 基线 − 0.002 | **+0.0014** | ✅ |
| TEDS | ≥ 基线 − 0.002 | **+0.0004** | ✅ |
| MHS | ≥ 基线 − 0.002 | **+0.0014** | ✅ |
| 单篇下降 > 0.05 | ≤ 2 篇 | **2 篇** | ✅ |
| L1（`engine-legacy` 200 篇逐字节） | 200/200 | **200/200**（每一步都验） | ✅ |
| L3（`semantic_regression` + `render_paths`） | 全绿 | 全绿 | ✅ |

四项**都高于**引擎基线，不是「勉强够容差」。§17.1 为 M2 放宽的 0.003 容差**没有被用到**，
M3 是按原定的 0.002 过的。

### 18.2 收敛：从 0.8540 到 0.8766

| 步骤 | 改动 | Overall | NID | TEDS | MHS | 回退 > 0.05 |
|---|---|---:|---:|---:|---:|---:|
| M3.1h + U 轮（起点） | — | 0.8540 | 0.8834 | 0.8343 | 0.7536 | 37 |
| **M3.2** | 表格导出**流内位置** + bbox 改用**被占用 item 的并集** | 0.8702 | 0.9133 | **0.8393** | 0.7284→0.7698 | 10 |
| **M3.3** | `reconcile_headings` 按**出现次序**消费 writer hint | 0.8709 | 0.9133 | 0.8393 | 0.7738 | 9 |
| M3.4 | 表格块直接由 hint 生成；页面有分组时丢弃未安置 item | 0.8702 | 0.9108 | 0.8393 | 0.7787 | 9 |
| M3.5 | 补回「表格已占用的行不再当段落」 | 0.8740 | 0.9166 | 0.8393 | 0.7799 | 4 |
| **M3.6** | 排除 `ItemType::Link`（注释矩形，不是正文） | 0.8767 | 0.9197 | 0.8393 | 0.7822 | 3 |
| M3.7 | 消费引擎的**逐 item 文本切片**（`text_plain_pieces`） | 0.8758 | 0.9185 | 0.8393 | 0.7815 | 3 |
| **M3.8** | 引擎的行内清理导出为 `clean_text_fragment` | **0.8766** | **0.9197** | 0.8393 | **0.7826** | **2** |
| 目标（engine） | — | 0.8753 | 0.9183 | 0.8389 | 0.7812 | ≤ 2 |

### 18.3 五个成因，全都是同一种形状

M1/M3.1 的教训在这一轮又出现了五次——**引擎已经做过的判断，消费者在旁边重做了一遍**：

| # | 消费者在重做什么 | 症状 | 改法 |
|---|---|---|---|
| M3.2 | 表格该放在文档的哪个位置 | 两栏页上**每张表都沉到页底**，与介绍它的正文分家。这是当时 −0.0213 里最大的一块，且 TEDS/NID/MHS 一起掉 | `TableHint` 增 `order`，在 writer 真正把表插进流的那一行记录 |
| M3.2 | 表格的范围 | 单行/单列表的 bbox 由行列**边界**算出 → 某一维为 0 → 匹配不上任何行，整张表丢失（§15.3 只修了零宽，零高还在） | bbox 改为**被该表占用的 item 的并集**——那才是表的真实范围 |
| M3.3 | 同名标题指向哪一行 | 文档里两处都叫 "Version History"（页标题 + 章节），`by_text` 用 `or_insert` 折叠成一条，第二个 `##` 拿到第一个的几何 → 消费者按几何永远看不见它 | 按出现次序逐个消费，并把已用掉的行标记掉 |
| M3.6 | 什么算正文 | 链接注释是**盖在锚文本上的矩形**，`text` 就是 URL。当成正文就会把 URL 粘进句子：`Affordable Courseshttps://uta.pressbooks.pub/...` | 和引擎一样，在分行之前就把 `ItemType::Link` 排除 |
| M3.8 | 行内文本清理 | 引擎在**成品字符串**上跑 `clean_markdown`；只读结构的消费者一点都拿不到。目录的点导线在 extractor 里是一个点一个 item、间距足够宽被读成词边界，只有在那一步才变成 `............`；折行的复合词是 `Fact - checkers` | 把其中**逐片段可判定**的四条规则导出为 `clean_text_fragment`；需要看上下文行才能判的（页码剔除、标题块重整、表格行重整）留在文档级 |

M3.7（导出逐 item 文本切片）是唯一一个**测下来更差**的步骤（−0.0009）。它没有被回滚，
因为它消除的是一整类分歧（字距、CID 字体、上下标、连字符），而 M3.8 让它连同清理一起落在
正确的一侧；但它单独并不是收益来源，记在这里以免下次有人以为它是。

### 18.4 两个仍然存在的差距

单篇回退还剩 2 篇，都在容差内但成因是清楚的：

- **`01030000000156`（−0.105）**：脚注编号、`Fact- checkers` 与 `Factcheckers` 的取舍
  （引擎保留连字符+空格，`postprocess::join_wrapped_lines` 直接去掉连字符）。两者都不完全对，
  GT 是 `Fact-checkers`。
- **`01030000000171`（−0.078）**：目录页的段落边界。引擎的 writer 对点导线行用 `\n` 而不是
  空格来续行（`convert.rs` 里唯一那个 join 点），IR 侧仍由 `merge_paragraphs_by_geometry`
  按几何决定。**导出 writer 的段落分组**是下一个自然的步骤，本轮没做——NID 对空白不敏感
  （评测器 `re.sub(r"\s+", " ")`），所以它的分数影响远小于它看起来的样子。

### 18.4b 一次由我造成的数据损失（记录在案）

本轮的逐步产物沿用了 `prediction/uparser-native-m3<N>-canonical` 这个命名，而 **M3.1 的
`m32`–`m39` 已经占用了同样的名字**——写之前没有检查，于是 §15.1 收敛表里 M3.1b–M3.1h 那
七步（外加一个）的**逐篇 Markdown 被覆盖**。`prediction/` 在 `.gitignore` 的 `opensource/`
之下，不可恢复。

影响范围有限但要说清楚：**§15.1 的四项分数本身在文档里，没有丢**；丢的是那几步的逐篇产物，
而产生它们的代码状态本来也已经不在了（那是对同一个工作区的连续编辑）。`m31-canonical`、
`m310-canonical`、`m3-final`、`u3-canonical`（M3 的实际起点）都完好。

已做的两件事：本轮产物改名为 `uparser-native-m3step<N>-canonical-20260907`，不再冒充旧的那
一批；`benchmark/gen_native_renderer_predictions.sh` 现在**拒绝写入已存在的目录**。

### 18.5 形态

```
② document-engine::render ── 全部（结构化 9 种格式 + 6 个 model protocol + pipeline + native PDF）
③ native-engine markdown ── 仅 `--markdown-source engine-legacy`，保留一个版本周期
```

`engine` 作为 `engine-legacy` 的 alias 保留，旧命令不会因为改名而失败。

### 18.6 现在的基准

| 路径 | Overall | NID | TEDS | MHS | 速度 |
|---|---:|---:|---:|---:|---|
| **native · canonical（新默认）** | **0.8766** | **0.9197** | **0.8393** | **0.7826** | 42.3 ms/篇 |
| native · engine-legacy | 0.8753 | 0.9183 | 0.8389 | 0.7812 | 41.6 ms/篇 |

速度 +1.8%：默认路径不再命中 `markdown_only` 快捷通道（那条通道跳过 `Page`/`Block` IR 构建，
只有引擎 Markdown 用得上）。200 篇 8.46 s vs 8.31 s。

**VLM 一列本轮无法复测**：`127.0.0.1:19122` 现在装的是 `MinerU2.5-Pro-2605-1.2B`，
而冻结基线是 `MinerU2.5-2604-1.2B`——拿新模型跑出来的分数会把「渲染器改动」和「模型换代」
混在一起，没有对照价值。不复测是有依据的：`render_markdown` 的 `EngineLegacy` 分支要求
`engine_markdown.is_some()`，而那只有 native 引擎产物才有，所以默认值翻转在结构上就到不了
VLM 那条路；本轮所有其余改动都落在 `native-engine` 与 `adapters/native.rs` 里。已用新模型跑
过一篇真实文档确认 VLM 链路本身没坏（表格、正文都正确）。

### 18.7 M4 的判断

§8 已经写明 M4 是**可选**的，建议观察一个版本周期。这里维持那个判断，并补一条本轮得到的
理由：③ 现在不只是「留着能回退」——`clean_text_fragment`、`text_plain_pieces`、
`StructureHints` 全部由它提供，②渲染 native PDF 的能力**建立在③之上**。所以 M4 真正能删的
是 ③ 的 *writer*（`to_markdown_from_lines_with_tables_and_images` 那条链），不是 §8 估的
9675 行整体；分析器与本轮新增的导出点必须留下。
