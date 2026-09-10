# mineru-vlm 适配器与官方 MinerU 的差距分析与改进方案

> 生成日期：2026-09-10
> 分析对象：`uparser/crates/uparser-core/src/adapters/mineru_vlm.rs` 及其上下游
> （`output_parse.rs` / `category_map.rs` / `imaging.rs` / `transport.rs` / `ascend.rs`）
> 对照基准：仓库内 `opensource/mineru-vl-utils` **2.0.1**（tag `mineru_vl_utils-2.0.1-released`，commit `1e56064`）、`opensource/MinerU`（mineru 3.4.4）
> 触发问题：OmniDocBench 上 `uparser · mineru-vlm` Overall **91.3490**，官方 MinerU2.5-Pro 参照 **95.75**，差 **-4.40**。是模型问题还是集成问题？

**结论：91.3490 对应的历史实现主要是集成问题，不是模型能力上限。`mineru_vl_utils` 2.0.1 没有改变本文涉及的 HTTP 请求协议、采样默认值、版面解析、类别集合或后处理逻辑，因此原归因仍成立。当前工作树已实现其中一批高优先级修复，但尚未重跑全量，不能把这些修复的预期收益写成已验证成绩。**

---

## 0. `mineru_vl_utils` 2.0.1 版本前提（重要）

`opensource/mineru-vl-utils` 当前已更新到 **2.0.1**。CLAUDE.md 中“只有 0.1.14 可用、更新的 `>=1.0.5` 系列本机没有”的记录已经过期；本文以仓库内可复核、可固定 commit 的 2.0.1 源码为准，不再以 Python 环境中偶然安装的包作为主基准。

2.0.1 的关键事实：

- `mineru_vl_utils/version.py` 明确为 `2.0.1`。
- `post_process/` 有 22 个 Python 文件（含 `__init__.py`；21 个功能模块），共 4355 行；`mineru_client.py` 为 1424 行。
- 从 tag `mineru_vl_utils-1.2.1-released` 到 2.0.1，`structs.py`、整个 `post_process/` 和 `vlm_client/http_client.py` **零差异**。
- `mineru_client.py` 的差异只涉及 Transformers 模型加载、LMDeploy 由私有 `VLAsyncEngine` 切到公开 Pipeline、以及把 MLX 纳入 stepping batch；不涉及 `DEFAULT_PROMPTS`、`DEFAULT_SAMPLING_PARAMS`、layout 正则、block 过滤、旋转或后处理调用。
- 2.x 的主要兼容性变化是 Transformers 5、LMDeploy 0.17、MLX 原生 batch 和新版 vLLM 依赖。uparser 当前走 OpenAI-compatible HTTP endpoint，这些本地推理后端变化不会改变适配器的 wire contract。

**问题根源仍未变化**：uparser 的初版 `mineru_vlm.rs` 是照着 0.1.14 写的，而 0.1.14 没有当前这套 `post_process/`。2.0.1 继续保留了完整后处理层，所以升级基准不会消除差距，反而确认这些处理是当前官方链路的一部分。

### 0.1 分析快照与当前工作树

本文的 91.3490 跑分和 A/B/C 缺口描述针对产生该跑分的历史实现。当前工作树存在尚未提交、尚未全量 benchmark 的修复：A1、A2、A3、B1、B2、B3、B4 中的 `inline_formula`/`unknown` 规则，以及 C1 已落到代码；其余项仍是缺口。下文保留原始问题说明，实施状态以第 5 节为准。

---

## 1. 为什么可以判定"不是模型问题"

三条独立证据：

1. **同一 checkpoint、同一 vLLM 端点**。uparser 与官方参照跑的是同一个 `MinerU2.5-Pro` 模型，差距只能来自模型之外。
2. **同一模型在 uparser 里曾达到过官方文本水平**。`BENCHMARK_REPORT.md` §1.1：历史配置 `mineru-vlm-2605-surpass-e1-full` 的 **Text Edit = 0.0367**，官方为 **0.036**。产生本文问题的后续运行退化为 0.0700 → 0.0837。这是 uparser 自身链路的回退，不是模型能力上限。
3. **跑分抖动本身就是 bug 的证据**。`BENCHMARK_REPORT.md` §3.2 记录 mineru-vlm 两次 Table TEDS 差 0.083，并解释为"VLM 采样非确定性"。但官方对**所有** prompt 类型都用 `temperature=0.0 / top_p=0.01 / top_k=1` 贪婪解码，**不应该存在任何非确定性**。该抖动直接指向下面的 A1。

触发本文分析的逐项差距（OmniDocBench，`BENCHMARK_REPORT.md` Part B §3.1 vs 官方参照；不代表当前未提交工作树）：

| 指标 | uparser mineru-vlm | 官方 MinerU2.5-Pro | 差值 |
|---|---:|---:|---:|
| Overall↑ | 91.3490 | 95.75 | **-4.40** |
| Text Edit↓ | 0.083655 | 0.036 | **+0.0477**（最大单项） |
| Formula CDM↑ | 92.1094 | 97.45 | -5.34 |
| Table TEDS↑ | 90.3032 | 93.42 | -3.12 |
| Order Edit↓ | 0.144774 | 0.120 | +0.0248 |

---

## 2. A 类：请求参数层（前处理）

### A1（P0）stage-2 根本没设 temperature，在用随机采样跑内容识别

产生 91.3490 的修复前实现中，`adapters/mineru_vlm.rs:115-142` 是：

```rust
"table" => ("\nTable Recognition:", json!({
    "presence_penalty": 1.0,
    "frequency_penalty": 0.005,
    "skip_special_tokens": false,
}))
```

三个分支（table / equation / default）**均无** `temperature` / `top_p` / `top_k` / `repetition_penalty`。`transport.rs:151-157` 把该 map 原样并入请求体，缺失字段由服务端补默认值——vLLM 的 OpenAI 兼容接口默认 `temperature=1.0, top_p=1.0`。

2.0.1 的 `mineru_vl_utils/mineru_client.py:34-56` 仍是：

```python
class MinerUSamplingParams(SamplingParams):
    def __init__(self, temperature=0.0, top_p=0.01, top_k=1,
                 presence_penalty=0.0, frequency_penalty=0.0,
                 repetition_penalty=1.0, no_repeat_ngram_size=100, ...)
```

`DEFAULT_SAMPLING_PARAMS` 的**每一个**条目（table / equation / image / chart / `[default]` / `[layout]` / `[cross_page_table_merge]`）都继承这些默认值，只覆盖 penalty。

**影响**：文本、表格、公式三类识别全部在非贪婪解码下产出，同时打击 Text Edit / Table TEDS / Formula CDM 三项，并完美解释跑分抖动。

**佐证**：`mineru_vlm.rs:357` 写着 `sampling["temperature"].as_f64().unwrap_or(0.0)`——那个 `unwrap_or` 正是在兜一个不存在的字段。stage-1 的 `layout_sampling()` 反而是对的（带 `temperature: 0.0`），说明这是移植遗漏而非有意设计。

### A2（P1）`no_repeat_ngram_size` 发错位置，等于没发

官方 `vlm_client/http_client.py:279-282`：

```python
if sp.no_repeat_ngram_size is not None:
    sp_dict["vllm_xargs"] = {"no_repeat_ngram_size": sp.no_repeat_ngram_size, "debug": self.debug}
```

必须包在 `vllm_xargs` 里才会被 vLLM 的自定义 logits processor 消费（官方自带 `logits_processor/vllm_v1_no_repeat_ngram.py` / `vllm_v0_no_repeat_ngram.py`）。uparser 在 `mineru_vlm.rs:104` 写成顶层字段，vLLM 会忽略。

**影响**：layout 阶段的防重复循环保护全程失效。uparser 用自己的 `robustness.rs`（事后检测重复→升温重试）来补，方向与官方相反——官方是解码时禁止，uparser 是事后升温，而升温本身又会降低质量。此外 stage-2 官方同样带 `repetition_penalty=1.0` 和 `no_repeat_ngram_size=100`，uparser 全无。

### A3（P1）90°/270° 旋转方向相反

- uparser `imaging.rs:46`：`image::imageops::rotate90`（**顺时针**）
- 官方 `mineru_client.py::prepare_for_extract`：PIL `block_image.rotate(block.angle, expand=True)`（**逆时针**）

180° 不受影响；90/270 的竖排/旋转文本块被送成差 180° 的图像。

### A4（P2）未启用 image_analysis 等 MinerU 默认开关

`opensource/MinerU/mineru/backend/vlm/vlm_analyze.py:222-240` 构造 client 时使用：

```python
enable_table_formula_eq_wrap=True,
image_analysis=True,
enable_cross_page_table_merge=True,
```

uparser 的 `SKIP_CONTENT` 把 `image` 完全跳过，因此 chart 永远不会被识别成表格/公式（官方 `post_process/image_analysis_postprocess.py` 会把 Image Analysis 结果二次分类为 `pure_table` / `pure_formula` / `chart` / `natural_image`）。

---

## 3. B 类：版面阶段（stage-1 解析）——结构性丢信号

### B1（P0）`merge_prev`（`txt_contd_tgt`）完全没解析

官方正则捕获尾部 tail 并调 `_parse_merge_prev(tail)`（`mineru_client.py:129-130`），`post_process/json2markdown.py` 据此把续段拼回上一段，规则是**中文不加空格、英文加一个空格**：

```python
if bbox_info.get("merge_prev", False) and last_text_contd_idx >= 0:
    if re.search(r"[一-鿿㐀-䶿]", content) is None:
        content = " " + content
    content_list[last_text_contd_idx] += content
```

uparser 的 `output_parse.rs::LayoutBox` 只有 `bbox_1000 / category_raw / angle`，tail 仅用于找 rotate token。

**影响**：跨栏、跨块、跨页的同一段落被切成多个 `\n\n` 段，直接对应 Text Edit 与 Reading Order Edit 两项回退——恰好是当前差得最多的两项。

### B2（P0）表格内部的 text/equation 块没被剔除

官方 `_filter_table_internal_layout_blocks`：凡 90% 落在 `table` 区域内的 `text` / `equation` / `equation_block` 一律丢弃。

uparser 只实现了 `absorb_image_block_members`（image 版），**没有 table 版**。

**影响**：表格里的文字既进了 HTML 表格、又作为正文段落重复输出一遍，Text Edit 与阅读顺序双输。

### B3（P1）`image_caption` 落在 image/chart/image_block 内时要删

官方 `prepare_for_extract` 开头即执行（`_find_covered_block_indices`，`candidate_types={"image_caption"}`, `container_types=IMAGE_CAPTION_CONTAINER_TYPES`）。uparser 无。

### B4（P1）类别词表落后于 2.0.1，其中 `inline_formula` 会造成正文重复

`category_map.rs:24-49` 的 `MINERU_VLM_CATEGORIES` 有 24 项；2.0.1 的 `structs.py::BLOCK_TYPES` 有 31 项。前者缺 `doc_title`、`paragraph_title`、`formula_number`、`index`、`caption`、`footnote`、`chart`。此外，`inline_formula` 虽不在 `BLOCK_TYPES` 中，却是官方 `parse_layout_output` 在类别校验前显式识别并丢弃的特殊模型输出，也必须纳入兼容逻辑。

- **`inline_formula`**：官方 `parse_layout_output` **显式 `continue` 丢弃**（行内公式已包含在文本块里）。修复前 uparser 将其映射为 `unknown`，再用 `Text Recognition:` 请求一次，导致同一公式重复；当前工作树已在映射前丢弃。
- **`chart`**：当前仍不在 `MINERU_VLM_CATEGORIES`/mapper 中，会归一化为 `unknown`；stage-2 也没有 `Image Analysis:` 分支。仅补词表而不实现 image analysis 仍不完整。
- **`formula_number` / `index` / 新增通用标题脚注类**：需要明确映射和渲染语义，不能继续依赖 unknown fallback。
- 官方还有一条 `unknown → image` 规则；当前工作树已实现。

### B5（P2）越界 bbox 处理与官方相反

官方 `_convert_bbox`：任何坐标 `<0` 或 `>1000` **直接返回 None 丢弃**，且 `x1==x2` / `y1==y2` 也丢。uparser 的 D.2 改成 clamp 到 1000 并保留。该改动当时基于"量化误差可恢复"的推理，与官方行为不一致，需 A/B 验证，不能想当然。

### B6（P2）逐行匹配 vs 全文 finditer

官方：`re.finditer(_layout_re, output, re.DOTALL)`。uparser：`for line in raw.lines()` + `^...$` 锚定。若模型把多个 box 放在同一行，uparser 严格正则失败、走 relaxed 正则**只捞到第一个 box**，其余静默丢失。

---

## 4. C 类：后处理层——缺口最大，也是 Formula CDM 差 5.3 分的直接原因

2.0.1 的 `mineru_vl_utils/post_process/` 共 22 个 Python 文件（含 `__init__.py`）、**4355 行**。uparser 移植的等价物仍只覆盖其中很小一部分。

| 官方模块 | 行数 | uparser 对应 |
|---|---:|---|
| `equation_delimeters` / `equation_left_right` / `equation_double_subscript` / `equation_fix_eqqcolon` / `equation_big` / `equation_leq` / `equation_unbalanced_braces` | ~1050 | 仅自写 `formula_repair.rs` 4 函数（`balance_brackets` / `collapse_repeated_quad` / `normalize_tag_eqno` / `rebuild_unclosed_env`），**无一是移植** |
| `equation_block.do_handle_equation_block` | 93 | 无（`equation_block` 进 `SKIP_CONTENT` 后丢弃） |
| `text_display2inline` / `text_inline_spacing` / `text_move_underscores_outside` | 114 | 无 |
| `_cleanup_non_text_placeholder_blocks`（清 `[Non-Text]`） | — | **无**，占位符原样进 Markdown |
| `table_image_processor`（表内图掩码 + token 回填 + 被吸收表图剔除） | 360 | 无 |
| `replace_table_formula_delimiters`（MinerU 开着 `enable_table_formula_eq_wrap=True`） | — | 无 |
| `cross_page_table`（跨页表合并，MinerU 开着） | 411 | 无 |
| `image_analysis_postprocess`（image → pure_table/pure_formula/chart 二次分类） | 334 | 无 |
| `mermaid_*` 4 个（chart→mermaid 的修复） | 1026 | 无 |
| `otsl2html` | 304 | `otsl.rs` 350 行（含测试），自写非移植 |
| `list_item → text`（1 行） | 1 | **相反**：uparser 映射到 `list`，`ascend.rs` 再渲染成 `- `，GT 无此 bullet |

另有两处语义级差异：

### C1（P0）页眉/页脚/页码/侧栏/脚注：官方丢弃，uparser 输出

`opensource/MinerU/mineru/backend/vlm/vlm_middle_json_mkcontent.py:359-416` 的 `mk_blocks_to_markdown` 只对 `TEXT / INTERLINE_EQUATION / PHONETIC / REF_TEXT / LIST / TITLE / IMAGE / TABLE / CHART / CODE` 有分支；`HEADER / FOOTER / PAGE_NUMBER / ASIDE_TEXT / PAGE_FOOTNOTE` **没有分支 → `para_text` 为空 → `continue` 丢弃**。

uparser `ascend.rs:180-230` 的 `ascend_block`：任何带 `text` 且非 `title` 的块一律 `DocBlock::Paragraph`，**没有任何按类别过滤**。

**影响**：每页的页眉、页脚、页码都作为正文段落进入 Markdown。1651 页几乎页页中招，是 Text Edit 的稳定噪声源。

### C2（P1）uparser 有、官方没有的两个处理，方向是净负

- **`content_normalize::normalize_punctuation`**：CJK 段落里把半角 `, . ; : ? ! ( )` 转全角。这是为解决 ODL 上一个具体观感问题加的，但 OmniDocBench 的 GT 是原文照抄，**每转一次就是一次编辑距离惩罚**。
- **`merge_paragraphs_by_geometry`**：官方从不按几何合并版面块，它用模型给的 `merge_prev`。uparser 用几何启发式代替了一个模型本来就提供的信号（见 B1），既不准又可能跨类别误合。

---

## 5. 实施方案与当前状态（按“改动量 ÷ 预期收益”排序）

每一步都应单独 A/B。这里的“已实现”只表示当前未提交工作树已有代码和单元测试，不表示 OmniDocBench 收益已确认。

| # | 项 | 当前状态 | 预期影响指标 | 下一步 |
|---|---|---|---|---|
| 1 | **A1** 补齐 stage-2 贪婪采样参数 | **已实现，未跑全量** | Text / Table / Formula | 先验证重复运行逐位一致，再跑全量 |
| 2 | **C1** Markdown/document-json 渲染前过滤 paratext | **已实现，未跑全量** | Text Edit | 检查 JSON 原始块仍保留，再跑全量 |
| 3 | **B1** 解析并消费 `txt_contd_tgt` / `merge_prev` | **已实现，未跑全量** | Text / Order | 核对跨页状态是否贯穿实际渲染边界 |
| 4 | **B2 + B3** table-internal 与 image-caption covered-block 过滤 | **已实现，未跑全量** | Text / Table | 增加真实 layout fixture 后跑全量 |
| 5 | **B4（部分）** 丢弃 `inline_formula`、`unknown → image` | **已实现，未跑全量** | Text | 补齐 2.0.1 类别表；单独设计 chart/image-analysis 路径 |
| 6 | **A2** `no_repeat_ngram_size` 放入 `vllm_xargs`，stage-2 同步启用 | **已实现，未跑全量** | 全部 | 确认服务端已注册 MinerU logits processor |
| 7 | **A3** 按 PIL 语义逆时针旋转 | **已实现，已有方向单测** | 旋转块 | 用真实 90°/270° 页面 A/B |
| 8 | **C（公式/文本）** 移植 7 个 `equation_*`、3 个 text 修复、`equation_block` | **未实现** | Formula / Text | 分模块移植并做上游 parity fixture |
| 9 | **A4/C（图表）** image analysis、表内图 token、跨页表、Mermaid | **未实现** | Table / Formula / Chart | 先确认 benchmark 口径是否启用这些开关 |
| 10 | **C2** mineru-vlm 路径关闭标点全角化和无信号几何合并 | **未实现** | Text | 做协议级开关，避免影响其他 adapter |
| 11 | **B5/B6** bbox 丢弃策略与全文 `finditer` 对齐 | **未实现** | Layout / Order | 先以真实坏样本验证，尤其避免放弃已有容错收益 |

第 1–3 项最可能贡献主要收益，因为它们分别对应采样质量、Text Edit（+0.048 是最大单项差距）与 Order Edit；但在全量结果出来前，不再表述为“足以达到 95.75”。

### 验证方法上的两个要点

1. **第 1 步做完，应立刻观察到同一语料两次跑分逐位一致**（消除非确定性）。这本身就是"修好了"的证据，比分数变化更硬。
2. 参照 `BENCHMARK_DEV_LOG.md` §3.4 的教训：**不要用分层子集的结论外推全量**。那次 prompt 实验在 290 页子集上三个变体全部显著提升，全量 1651 页却反转为净负，根因是子集抽样没覆盖 `watermark`/`fuzzy_scan`/`traditional_chinese` 等受害类目。本方案的每一项都应直接跑全量确认。

---

## 6. 局限与未验证项

- 本文所有偏差均已对 `mineru_vl_utils` **2.0.1** 做代码级复核，但每一项对分数的具体贡献仍是假设，需按第 5 节逐个 A/B 才能定量。
- B5（越界 bbox 丢弃 vs clamp）**方向未定**：官方丢弃、uparser clamp，哪个在当前 checkpoint 上更好需要实测，本文不预设结论。
- 已核查 1.2.1 → 2.0.1：本文相关的 `structs.py`、`post_process/`、HTTP client 均无变化，`mineru_client.py` 只改本地后端装载/批处理。因此 2.0.1 没有推翻本文的协议与后处理结论。
- “官方行为”仍取决于上层 MinerU 实际传给 `MinerUClient` 的开关（如 `image_analysis`、`abandon_paratext`、`enable_cross_page_table_merge`）；比较时必须记录调用配置，不能只记录包版本。
- 官方 95.75 是**公开榜单参照值**，非本机复现；本机原始 MinerU 复跑（`BENCHMARK_REPORT.md` §1.3 口径）才是严格同环境 A/B。若要把“差 4.40”归因得更干净，应先固定 `mineru_vl_utils` **2.0.1**、上层 MinerU 开关、同一 checkpoint 和同一 vLLM 端点跑一遍官方链路，拿到本机官方基线再逐项消融。
