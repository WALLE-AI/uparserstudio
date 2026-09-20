# 对齐 `monkeyocr-v2` 适配器与上游 MonkeyOCRv2 参考实现

> 状态：**已实施并完成两榜全量重测**（2026-09-18/19）。结果见文末 §7；
> 过程与归因见 `BENCHMARK_DEV_LOG.md` §4；榜单数字见 `UPARSER_LEADERBOARD.md`。
> 范围：`uparser/crates/uparser-core/src/adapters/monkeyocr_v2.rs`、
> `uparser/crates/uparser-core/src/monkeyocr_post.rs`，对齐基准为
> `opensource/MonkeyOCRv2/parsing/core_runner.py`

## 1. 背景与动机

`uparser` 的 `monkeyocr-v2` 协议是对上游 `core_runner.py` 的移植。当前它在两个榜单上
都明显落后于同类 VLM 协议，且差距远大于模型本身应有的差距：

| 榜单 | 当前 monkeyocr-v2 | 同 harness 下的 mineru-vlm |
|---|---|---|
| opendataloader-bench Overall ↑ | 0.8754（垫底，8.639 s/篇） | 0.9252（0.682 s/篇） |
| OmniDocBench Text Edit ↓ | 0.1408 | 0.0837 |
| OmniDocBench Table TEDS ↑ | 0.8336 | 0.8797 |
| OmniDocBench Formula Edit ↓ | 0.2747 | 0.1042 |

而官方 README 的 OmniDocBench v1.6 端到端榜上 **MonkeyOCRv2-B-Parsing 以 83.3 位列第一**
（超过 dots.mocr 80.5、chandra-ocr-2 79.7、MinerU-2.5-Pro 71.0）。本地 `127.0.0.1:8011`
跑的正是 `MonkeyOCRv2-B-Parsing` 官方权重（已确认在线）—— 也就是说这个差距
**来自适配器，不来自模型**。

逐行比对 `core_runner.py` 后确认了 8 处真实偏差（第 2 节，每条都能指到上游具体行为），
其中第 1 条（识别裁剪图被我们放大到 100 万像素）和第 4 条（OTSL 单元格内任意 `<tag>`
被误当成控制 token）两条足以独立解释大部分文本/表格分数损失。

**目标**：把适配器对齐到上游参考实现，然后用同一 harness 重测，把榜单数字推到与官方
口径一致的量级。

## 2. 已确认的偏差清单

证据一律指向 `opensource/MonkeyOCRv2/parsing/core_runner.py`。

| # | 项 | 上游行为 | 我们当前 | 影响 |
|---|---|---|---|---|
| 1 | **识别阶段裁剪图缩放** | `batch_inference(..., min_pixels=None)` + `MOCR2_MAX_PIXELS=1003520`（`configure_runtime`、`BackendConfig.max_pixels`）→ `load_image` **只缩小、绝不放大** | `resize_by_pixel_bounds(&crop, TARGET_PIXELS, TARGET_PIXELS)` → 一行文字的小裁剪被 Lanczos **放大近 10 倍**送进模型 | 正文/公式识别质量（Text Edit、Formula Edit） |
| 2 | 极端长宽比 | `load_image`：`max/min > 200` → 换成 32×32 空图 | 无此保护 | 偶发脏输出 |
| 3 | 识别 `max_tokens` | `5000`（`_parse_page`、`_recognize_one_block`） | `10000` | 口径一致性 |
| 4 | **OTSL token 化** | 只有 `fcel｜ecel｜lcel｜ucel｜xcel｜nl` 六个控制标签是 token，**大小写不敏感**；单元格里的 `<br>`／嵌套 `<table>`／`<b>` 属于内容 | 正则 `<([a-z]+)>` 把**任意小写标签**当控制 token → 含 `<br>` 的单元格直接打乱整张表格网格 | Table TEDS |
| 5 | OTSL 其他细节 | ① `<otsl>…</otsl>` 包裹递归剥离（`re.I｜re.S`）；② 私有转义 ``→``、`tag>`→`<tag>`；③ `fcel` 内容**不做 strip**；④ 单元格渲染保留内嵌 HTML 标签，仅对纯文本块转义，`&` 用实体负向 lookahead，另转义 `"`／`'`，`<br/>` 归一为 `<br>` | 均无；且做了 `content.trim()`、整串 `escape_html` | Table TEDS |
| 6 | 表内图片标记 | `[img][x1,y1,x2,y2][/img]`（相对表格裁剪图的 0–1000 坐标）→ 裁剪并替换为 `<img src=… alt="embedded table image" />`（`_replace_table_image_markers`） | 不处理，标记文本原样漏进 HTML | Table TEDS |
| 7 | 重复退化重试 | `detect_repeat_token`（后缀重复法：`base_max_repeats=4`、`window_size=500`、`scaling_factor=3.0`）+ 去尾 50 字符再判一次；**`PipelineConfig.retry_repeat` 默认 False**；开启时对**所有** `need_infer` 标签（含 Table/Formula）生效，`temperature=min(0.2*(n+1),0.8)`、`top_p=0.95`、最多 3 次 | 自研 `robustness::is_degenerate`（滑窗周期法），**默认开启**，且只对非 Table/Formula 生效，无 `top_p` | 口径一致性 + 耗时（8.639 s/篇偏高的一个来源） |
| 8 | 原始输出裁边 | `_format_block_fields`：`content = (raw or "").strip()` | 文本分支只做 `strip_replacement_chars`，不 trim | 尾随空白 |

### 2.1 已经是对齐的，不要动

布局阶段 min=max=1003520（等价上游）、`_map_bbox_to_image` ≡
`geometry::map_bbox_0to1000_clamped`（四舍五入 + `width-1` 夹取 + `x2≥x1+1`，已逐行核对）、
容错 literal 解析（`output_parse::parse_python_literal_list` ≡ `_parse_tolerant_items`）、
`process_formula`（含刻意保留的 `group(1)` 行为）、`Picture` 跳过推理但仍裁图存资产、
`Footnote` 因被注释出 `ALL_PROMPT` 而 `need_infer=False`、`Title`→H1 ／
`Section-header`→H2、`Page-header`／`Page-footer` 不进 Markdown
（`ascend::ParatextPolicy` 默认丢弃 ≡ `keep_header_footer=False`）、无 system message、
PNG data URI、布局 `max_tokens=4096`、模型输出顺序即阅读顺序（上游不排序）。

### 2.2 刻意不做

- 文档摆正预处理（`modeling_preprocessor.Preprocessor`，独立 torch 模型，延续 T-3.5 的 option a）
- `end2end` 单次调用模式（上游 `PipelineConfig.end2end` 默认 False）
- `MOCR2_TABLE_HTML=1` 分支（上游默认 `0`，即 OTSL→HTML 路径）

## 3. 实施

### 3.1 `monkeyocr_post.rs`

- **重写 `otsl_to_html`**，覆盖 #4／#5：
  - 入口先做 `<otsl>…</otsl>` 大小写不敏感递归剥离；
  - token 化改为只识别六个控制标签（大小写不敏感），内容切到下一个控制标签为止 ——
    现有"定位标签再切片"的做法保留（`regex` crate 无 lookahead），只把标签白名单收紧；
  - 加私有转义解码；去掉 `content.trim()`；
  - 单元格渲染改为上游的"按 `(<[A-Za-z][^>]*>)` 切片、奇数块原样透传（`<br/>`→`<br>`）、
    偶数块转义"逻辑。这需要一个**本模块私有**的转义函数（`&` 带实体 lookahead + `<>"'`），
    **不改共享的 `otsl::escape_html`** —— mineru-vlm／dots-ocr／pipeline 都在用它；
  - `lcel`／`ucel`／`xcel` 的网格推导逻辑已与上游一致，保持不变。
- **新增 `detect_repeat_token` / `should_retry_repeat_output`**（#7 的判定部分），参数与上游一致。
  注意上游用 `str.endswith(seq * (max_repeats+1))` 做后缀判定，Rust 侧按**字符**（非字节）
  切片以免 UTF-8 越界。
- **新增 `TABLE_IMAGE_PATTERN` + `replace_table_image_markers`**（#6）。采用上游
  `use_base64=True` 分支，产出 `<img src="data:image/png;base64,…" alt="embedded table image" />`
  —— 因为 `Block` 只有单个 `asset_bytes`／`asset_path`，一张表里多张内嵌图无法用现有 IR 表达，
  而 base64 分支本身就是上游提供的合法选项，不需要改 IR。相同 bbox 只裁一次（上游 `refs` 缓存）。

### 3.2 `imaging.rs`

新增 `prepare_model_image(img, min_pixels: Option<u32>, max_pixels: Option<u32>) -> RgbImage`，
按 `load_image` 的顺序执行：先 min 放大 → 再 max 缩小 → 最后 `max/min > 200` 换 32×32 空图。
复用现有 `resize_by_pixel_bounds` 的 sqrt 缩放，不重写缩放本身。

### 3.3 `adapters/monkeyocr_v2.rs`

- 布局阶段：`prepare_model_image(page, Some(1003520), Some(1003520))`（行为不变，额外获得 #2 保护）。
- 识别阶段：`prepare_model_image(crop, None, Some(1003520))` —— 修掉 #1，本次最关键的一行。
- 识别 `max_tokens` 10000 → 5000（#3）。
- 所有分支在格式化前先 `trim()` 原始输出（#8）。
- `Table` 分支：`otsl_to_html` 结果再过 `replace_table_image_markers(裁剪图)`（#6）。
- 重试（#7）：改用 `monkeyocr_post::should_retry_repeat_output`，**默认关闭**（与上游
  `retry_repeat=False` 逐位一致）；开启时对所有 `need_infer` 标签生效，温度
  `min(0.2*(n+1),0.8)`、`top_p=0.95`、最多 `retry_repeat_max_retries`(3) 次。
  现有 `robustness::retry_with_temperature` 的终止条件写死在 `is_degenerate` 上、
  且不支持 `top_p`，与上游判定不同，因此改为本适配器内的显式循环；
  `robustness.rs` 本身不动（mineru-vlm 仍在用它）。

### 3.4 配置管线（沿用 `NavidcConfig` 的既有模式）

`adapters/mod.rs` 新增 `MonkeyOcrConfig { retry_repeat: bool, retry_repeat_max_retries: u32 }`
与 `AdapterOverrides.monkeyocr`，registry 工厂里应用；`cli.rs`／`runner.rs`／`api.rs` 按
`navidc_config` 的同一路径透传，新增 CLI 开关
`--monkeyocr-retry-repeat` ／ `--monkeyocr-retry-repeat-max-retries <N>`。

## 4. 测试

延续本仓库"golden 值由**执行上游 Python** 抓取、而非推理得出"的既有纪律
（见 `monkeyocr_post.rs` 现有测试注释）：用 `python3 -c 'import core_runner'` 直接调
`otsl_to_html` / `detect_repeat_token` 抓 golden，写进断言并注明来源。

- `monkeyocr_post`：`<br>` 单元格、嵌套 `<table>`、`<otsl>` 包裹、私有转义、空白保留、
  实体保留（`&amp;` 不被二次转义）、`'`／`"` 转义；`detect_repeat_token` 正反例；
  `[img]` 标记替换（含同 bbox 复用）。保留现有 proptest "永不 panic"。
- `imaging::prepare_model_image`：小图在 `min_pixels=None` 下**尺寸不变**（#1 的回归网）、
  大图被缩小、长宽比 >200 变 32×32。
- `monkeyocr_v2`：小裁剪块送出的图片尺寸等于裁剪尺寸（`MockDispatch` 断言请求体里的图片尺寸）、
  `max_tokens=5000`、默认**不**重试（只喂一个退化响应且不喂第二个 seed，多余 dispatch
  会因 mock 查不到而失败 → 证明默认关闭）、开启后按上游温度序列重试。
- 现有 6 个 `monkeyocr_v2` 测试与 `tests/contract.rs` 的跨协议一致性测试必须继续通过。

## 5. 验证

1. `cargo test --workspace`、`--features native`、`clippy --all-targets -D warnings`、`fmt --check`。
2. 真端点冒烟：`127.0.0.1:8011`（`MonkeyOCRv2-B-Parsing`）单页 + 单份多页 PDF，
   对比改前／改后 Markdown，确认表格与正文向好、`page_errors`／`warnings` 为空。
3. **小样本先行**：从 `benchmark/OmniDocBenchData` 取 ~50 页固定子集，用 `benchmark/gen_monkey.sh`
   的同一命令行跑改前／改后两份预测，跑 `OmniDocBench/run_eval.py` 对比
   Text Edit／Formula Edit／Table TEDS／Reading Order。**只有确认为正向收益才继续第 4 步。**
4. 全量重测：OmniDocBench 1651 页 + `opensource/opendataloader-bench` 200 篇
   （release 构建、`--no-cache --no-assets`，与榜单口径一致）。
5. 更新 `UPARSER_LEADERBOARD.md`：刷新 monkeyocr-v2 两行数字，并在"外部参照"里补一行
   MonkeyOCRv2-B-Parsing 官方发布值（README OmniDocBench v1.6 端到端 83.3，注明该分数
   为 0–100 综合分、与本表 edit／TEDS 口径不同，仅作量级参照）。过程与归因写进
   `BENCHMARK_DEV_LOG.md`，`CLAUDE.md` 追加本批次记录。

## 6. 风险

- **#1 取消放大后**，极小裁剪块（如单个页码）分辨率偏低可能反而变差 —— 第 5 节小样本
  对比就是为此设的闸门；若确认负向，改为"仅对面积小于某阈值的块保留上采样"，
  并在文档里明确标注为**刻意偏离上游**。
- **#7 默认关闭后**，模型偶发死循环输出将不再被客户端救回（与官方默认一致）；
  CLI 开关保留，`BENCHMARK_DEV_LOG.md` 里记清这一权衡。
- `otsl_to_html` 重写只影响 `monkeyocr-v2`（专属模块），共享 `otsl.rs` 不动，
  其他协议的快照／契约测试不应有任何变化 —— 若有变化即为实现出错的信号。

---

## 7. 实施结果（2026-09-18/19）

### 7.1 代码

全部 8 项偏差已按第 3 节实施。新增/改动：

| 文件 | 内容 |
|---|---|
| `imaging.rs` | 新增 `prepare_model_image(img, min_pixels, max_pixels)`，忠实移植 `load_image` 的"min 只上采样 → max 只下采样 → 长宽比 >200 换 32×32 黑图"序列；`resize_by_pixel_bounds` 的边长改为截断（对齐 `int()`） |
| `monkeyocr_post.rs` | `otsl_to_html` 按上游重写（控制标签白名单、`<otsl>` 递归、私有转义、不 trim、保留内嵌 HTML 的转义）；新增 `detect_repeat_token` / `should_retry_repeat_output` / `replace_table_image_markers` |
| `adapters/monkeyocr_v2.rs` | 两阶段分别用正确的像素边界；识别 `max_tokens` 5000；输出先 trim；表格过 `[img]` 标记替换；重试改用上游算法且**默认关闭** |
| `adapters/mod.rs`、`cli.rs`、`runner.rs`、`api.rs` | 新增 `MonkeyOcrConfig` 与 `--monkeyocr-retry-repeat` / `--monkeyocr-retry-repeat-max-retries`，沿用 `NavidcConfig` 既有路径，并计入缓存键 |

测试：全 workspace 绿（`uparser-core` lib 510、doc-engine 888、CLI/contract 全通过），
`cargo fmt --check` 干净。新增用例集中在三处：`prepare_model_image` 的"小图不被放大"回归网、
`monkeyocr_post` 的上游 golden（含两个刻意复现的 quirk）、适配器层的"裁剪图按原尺寸送出 +
`max_tokens=5000` + 默认不重试 + 开启后按上游温度序列重试（含表格）"。

### 7.2 opendataloader-bench（200 篇，同 harness 重跑两侧）

| | Overall ↑ | NID ↑ | TEDS ↑ | MHS ↑ | s/篇 ↓ |
|---|---:|---:|---:|---:|---:|
| 对齐前 | 0.8754 | 0.8917 | 0.9085 | 0.8200 | 8.639 |
| **对齐后** | **0.8827** | **0.8932** | **0.9522** | **0.8326** | **3.011** |

TEDS +0.0437 直接对应 §2 的第 4/5 条（OTSL 分词与转义）；速度 2.9 倍主要来自不再把小裁剪图
放大约 10 倍（token 数骤降），以及不再触发复读循环。

### 7.3 OmniDocBench v1.6（全量 1651 页，uparser CLI 逐页）

| 指标 | 对齐前 | 对齐后 | 变化 |
|---|---:|---:|---|
| Text Edit ↓ | 0.1408 | **0.0834** | −41% |
| Formula Edit ↓ | 0.2747 | **0.1990** | −28% |
| Formula CDM ↑ | 0.8050 | **0.8981** | +0.093 |
| Table TEDS ↑ | 0.8336 | **0.8373** | +0.004 |
| Table TEDS-S ↑ | 0.8715 | **0.8745** | +0.003 |
| Reading Order Edit ↓ | 0.1922 | **0.1549** | −19% |

两侧均取 `result/*_metric_result.json` 的 `all` 字段（与榜单历史口径一致）。

### 7.4 仍然存在的缺口

- **文档摆正预处理**仍未实现（§2.2），上游默认是开启的。对拍摄/倾斜页面，我们仍比上游少一步。
- **表内 `[img]` 标记**走的是上游 `use_base64=True` 分支（内联 data URI），不是写文件分支 ——
  因为 `Block` 只有一个 `asset_bytes`/`asset_path`，一张表里多张内嵌图无法用现有 IR 表达。
- **`Title`/`Section-header` 的多行前缀**：上游对每一行都加 `# `/`## `，我们用
  `MergeHint::TitleLevel` 交给渲染器，单行标题等价，多行标题不等价。
- **`MOCR2_TABLE_HTML=1` 分支**未实现（上游默认 `0`，即 OTSL→HTML）。
