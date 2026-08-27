# Pipeline V2 · Rust CLI staged 精度缺陷分析与优化执行方案

> **版本**:v3(2026-08-27,S1/S2/S3 已实现)。v1 的主干结论(表格 OCR 断链)成立并保留;
> v2 复核对照真实代码与榜单数据,证伪了 v1 的两条结论、补充了五处遗漏缺陷,并修正了
> 「能否超过官方」的判断。v3 实现了 S1(表格专属 OCR 分流)、S2(词表/行内公式/去重/
> 标题层级)、S3(MinerU 3.4.5 拆分为五个纯推理 stage endpoint)——**代码已写、单元/离线
> 测试已通过,但均未在真实评测数据集上重新跑分**(本会话没有可用的 GPU/MinerU 运行环境,
> 见 §5)。以榜单数据为准,凡与旧版冲突处以本版为准。
>
> **背景**:`uparser Pipeline V2 · Rust CLI staged` 在 OmniDocBench 上 Overall 从
> `75.7789` 跌到 `58.6162`,Table TEDS 从 `73.1073` 跌到 `27.5343`,但结构指标 TEDS-S
> 仍有 `84.7622`。
>
> **目标**:优化 `uparser/crates/uparser-core/src/adapters/pipeline_v2.rs`,使精度超过官方
> MinerU-Pipeline(Omni Overall `86.47`)。
>
> **数据来源**:`BENCHMARK_REPORT.md` 第 1.2 / 1.2.1 / 2.5 节、
> `benchmark/results/pipeline_comparison_20260825.md`、
> `benchmark/results/pipeline_v2_accuracy_20260825.md`。

---

## 0. 一页结论

1. 本次 `58.62` 相对旧 `75.78` 的回退,**全部来自 Rust 侧编排与渲染,与模型权重无关**——
   两次跑的是同一套 legacy 模型。回退可拆成:表格 OCR 断链(主因)+ 四处编排/渲染不一致。
2. 但**修完这些编排缺陷的上限只有 ~76**。原始 MinerU 用同一套 legacy 权重也只跑到
   `76.13`。**没有任何编排层改动能把 legacy 模型栈推到 `86.47`。**
3. 因此「超过官方」的**必要条件**是把 MinerU 3.4.5 的模型(PP-DocLayoutV2 / PP-OCRv6 /
   PP-FormulaNet-plus-M / 新表格链)真正接到 5 个 standalone stage endpoint 上——目前它们
   **无条件绑死在 legacy 链**,`pipeline_v2.rs` 一次都碰不到,本地模型下载与否都无影响。
4. 真正的目标锚点不是官方的 `86.47`,而是**本机已验证的 MinerU 3.4.5 + PP-Formula
   `88.51`**——同机同评测器,才是公平且可超越的基线。

### 关键数据(OmniDocBench,Overall 越高越好)

| 路径 | 模型栈 | Overall | 文本 Edit↓ | 公式 CDM↑ | 表格 TEDS↑ | TEDS-S↑ | 顺序 Edit↓ |
|---|---|---:|---:|---:|---:|---:|---:|
| 原始 MinerU(同 legacy 权重) | legacy | 76.1250 | 0.153716 | 68.6217 | 75.1248 | — | 0.266186 |
| Python V2(HTTP `pages:analyze`) | legacy | 75.7789 | 0.190804 | 73.3099 | 73.1073 | 84.7475 | 0.294770 |
| **Rust staged(本次)** | legacy | **58.6162** | **0.217967** | **70.1110** | **27.5343** | **84.7622** | **0.288339** |
| 本地 MinerU 3.4.5 | 3.4.5 | 85.3391 | 0.056176 | 79.5826 | 82.0523 | 88.8441 | 0.153534 |
| 本地 3.4.5 + PP-Formula | 3.4.5 | **88.5123** | 0.054230 | 88.5788 | 82.3811 | 88.8515 | 0.147369 |
| 官方 MinerU-Pipeline(参考) | — | 86.47 | 0.055 | 83.07 | 81.88 | 88.68 | 0.153 |

**读法**:legacy 三行(76.13 / 75.78 / 58.62)之间的差异 = 编排质量;legacy 组与 3.4.5 组
之间的 ~10 分鸿沟 = 模型能力,编排改不动。

---

## 0.5 架构约束(硬性,决定各阶段的落点)

> **除模型推理服务外,集成代码与后处理代码一律用 Rust 实现。**

据此明确职责边界:

| 层 | 语言 | 允许做的事 | **禁止**做的事 |
|---|---|---|---|
| 模型服务(`services/pipeline-model-server`) | Python | 单 stage 推理:收图/收区域 → 跑模型 → 回结构化结果(region / span / latex / cell) | 跨 stage 编排、区域归属、阅读顺序决策、Markdown 装配、文本规范化 |
| 集成与后处理(`uparser-core`) | **Rust** | stage 调度与并发、区域合并去重、表格/公式/文本内容装配、阅读顺序、类目映射、Markdown/JSON 渲染、文本规范化 | 无 |

对现状的直接判定:

- `page_analyzer.py::PipelinePageAnalyzer`(Python 侧的 5 阶段编排)属于**违反约束的实现**,
  只能作为行为参照物读,不能作为产品路径,也不应继续演进。它的正确归宿是被
  `pipeline_v2.rs::run_workflow` 完整替代——本文档 S1/S2 做的正是这件事。
- `mineru345_backend.py` 现有的 `pages_analyze` / `documents_analyze` 后端直接调 MinerU
  `do_parse` 产出 Markdown(`mineru345_backend.py:204-260`),**同时越过了编排层与渲染层**,
  同样违反约束。它产出的 `85.3391` / `88.5123` 只能作为**参考基线分**,不能作为产品形态。
- `run_pipeline_v2_benchmarks.py::render_markdown`(Python 渲染器)是评测脚本的临时产物;
  产品渲染器只有一个,即 Rust 的 `render::to_markdown`。凡是 Python 渲染器有而 Rust 没有的
  行为(行内公式 `$…$`、`doc_title`/`paragraph_title` 分级、按 `reading_order` 去重遍历),
  都应视为 **Rust 侧的缺口**(即 D3 / D6),而不是「照抄 Python 就行」。

因此 S3 的措辞必须收紧:它**不是**「把 MinerU 3.4.5 接进来」,而是
「**把 3.4.5 的五个模型拆成纯推理的 stage endpoint,编排仍全部留在 Rust**」。

---

## 1. 缺陷清单

### D1(致命,主因):`run_workflow` 缺少表格专属 OCR 调用

`pipeline_v2.rs:591-639` 只发起**一次**页面级 OCR,并把结果直接喂给表格阶段:

```rust
let ocr = self.dispatch_stage::<_, OcrResult>(..., OcrPageInput {
    layout_regions: layout.regions.clone(), ...
});
// ...
TableRecognitionInput {
    ocr_spans: ocr.spans.clone(),     // ← 缺陷点
    ...
}
```

服务端 `recognition_backends.py:27-46` 的 `TEXT_REGION_LABELS` **不含 `"table"`**,
`LegacyOcrBackend` 天生跳过表格区域(这本身是对的,MinerU 页面级 OCR 就不处理表格)。
因此 `ocr.spans` **结构性地不可能包含表格内文字**。`table_backend.py::_table_ocr()` 按
span 中心点落在表格 bbox 内筛选,几乎筛不到东西,SLANet-plus 收到 `ocr_result=[]`——
单元格结构框(`cell_bboxes` / `logic_points`)仍能预测,但 `_cell_text()` 匹配不到任何
span,**单元格文字全丢**。

这与「TEDS-S `84.76` 完好、TEDS `27.53` 崩溃」的强烈分化完全吻合:结构对、内容空。

**对照实现已存在**:同一服务的 `page_analyzer.py:137-166`(旧 `75.78` / `0.858` 走的
finalize 路径)专门为表格区域**再发一次 OCR**,把 label 临时改成 `"text"` 绕过
`TEXT_REGION_LABELS` 过滤,并把**这次**的 span 喂给表格模型:

```python
table_regions = [r for r in regions if r.label == "table"]
table_ocr = self._infer("ocr", OcrPageInput(
    layout_regions=[r.model_copy(update={"label": "text"}) for r in table_regions],
    formula_regions=formula_regions, language=item.language, page=item.page,
))
table_result = self._infer("table", TableRecognitionInput(
    table_regions=table_regions,
    ocr_spans=table_ocr.spans,        # 不是 ocr.spans
    formula_spans=formula_result.spans, page=item.page,
))
# 最终 ocr_spans = ocr.spans + table_ocr.spans
```

`pipeline_v2.rs` 从未实现这一步。

---

### D2:类目词表与服务端实际输出对不上,所有公式区域退化为 `unknown`

服务端 `compat.py:12-28` 实际发出的 label 集合:

| 来源 | label |
|---|---|
| layout(`LEGACY_LAYOUT_LABELS`) | `paragraph_title` `text` `discarded` `image` `figure_title` `table` `vision_footnote` `display_formula` `formula_number` |
| MFD(`LEGACY_MFD_LABELS`) | `inline_formula` `display_formula` |

`category_map.rs:208-222` 的 `map_pipeline_category` 认识的键:
`doctitle` `paragraphtitle` `verticaltext` `text` `abstract` `image` `table` `chart`
`interlineequation` `list` `index` `header` `footer` `pagenumber` `footnote` `discarded`。

交集之外的 **`display_formula` / `inline_formula` / `figure_title` / `vision_footnote` /
`formula_number` 全部落到 `"unknown"`** 并逐个触发 `ctx.warn`。两套词表从未对齐过——
`map_pipeline_category` 是按 MinerU `enum_class.py::BlockType`(PP-DocLayoutV2 词表)写的,
而当前 standalone endpoint 跑的是 legacy 链、发的是 PDF-Extract-Kit 词表。

> 注:这是 D5 的直接后果之一——一旦 S3 换成 3.4.5 后端,词表会变成 PP-DocLayoutV2 的,
> 届时需再次校验,不能只按 legacy 词表打补丁。

---

### D3:行内公式被渲染成块级公式,切断段落

`render/mod.rs:19-22` 对任何非空 `latex` 一律输出:

```rust
out.push_str("$$\n"); out.push_str(latex); out.push_str("\n$$\n\n");
```

而 Python 对照实现(`run_pipeline_v2_benchmarks.py::render_markdown`)区分:

```python
blocks.append(f"${latex}$" if label == "inline_formula" else f"$$\n{latex}\n$$")
```

于是每个行内公式在 Rust 路径里都会把所在段落**从中间切断成独立 display 块**。这同时解释
了 Text Edit `0.1908 → 0.2180`(+0.027,变差)与 Formula CDM `73.31 → 70.11`(-3.2)两项
同向退化。

---

### D4:`merge_layout_and_mfd` 的去重逻辑未移植,残留空白幽灵块

Python `compat.py:107-122` 用 `intersection_over_min_area >= 0.7` 把 **layout 自己检出的
`display_formula` / `inline_formula` 区域删除**,只保留 MFD 版本:

```python
for region in layout_regions:
    if region.label in {"inline_formula", "display_formula"} and any(
        intersection_over_min_area(region, f) >= 0.7 for f in formulas):
        continue
    merged.append(region)
merged.extend(formulas)
```

`pipeline_v2.rs:641-653` 的去重只有两个条件:公式区域中心是否落在表格里
(`bbox_contains_center`)、`region_id` 是否重复(而 `layout-N` 与 `mfd-N` **永不冲突**)。
结果 layout 版公式区域全部残留:它们既没有 OCR 文字(不在 `TEXT_REGION_LABELS`)、也没有
latex(MFR 只识别 MFD 输出的区域),渲染时四个分支全不命中 → 变成**空白幽灵块**,
占据阅读顺序槽位并干扰 XY-cut 的投影切分。

---

### D5(决定天花板):standalone stage endpoint 无条件绑死 legacy 链

`app.py:128-154`:

```python
profile = os.getenv("UPARSER_PIPELINE_PROFILE", "mineru-3.4.5").strip().lower()
registry = BackendRegistry()
register_pipeline_backends(              # ← 无条件执行
    registry, manifest, device,
    include_page_analyzer=profile == "legacy",
)
if profile == "mineru-3.4.5":
    register_mineru345_page_backend(registry, source_root, config_path, device)
```

`register_pipeline_backends`(`legacy_backends.py:198-280`)把
`layout` / `formula_detect` / `ocr` / `formula_recognize` / `table` 五个 stage
**永久绑定到 legacy 权重**:DocLayout-YOLO、YOLOv8-MFD、旧 PaddleOCR-torch、
UniMERNet-small、SLANet-plus(仅无线表格)。

`register_mineru345_page_backend`(`mineru345_backend.py:263-297`)只注册
`pages_analyze` 与 `documents_analyze` 两个**整页/整文档 finalize** 后端——它内部直接调
MinerU 的 `do_parse` 出 Markdown(`mineru345_backend.py:204-260`),**不暴露任何分阶段接口**。

`pipeline_v2.rs` 只调用那 5 个 standalone endpoint,因此:

- 本地已下载的 PP-DocLayoutV2 / PP-OCRv6 / PP-FormulaNet-plus-M / PP-LCNet 表格分类 /
  UnetStructure 有线表格,`pipeline_v2.rs` **一个都用不到**;
- `UPARSER_PIPELINE_PROFILE=mineru-3.4.5` 对 Rust 分阶段路径**没有任何作用**;
- 这是一条无法通过 Rust 侧优化绕过的结构性天花板。

---

### D6:标题层级信息被算出来但无人消费

`pipeline_v2.rs:857-861` 计算了 `MergeHint::TitleLevel(1)` / `TitleLevel(2)`,但
`render/mod.rs:32-42` 只读 `block.category`,`merge_hint` 从未被渲染层消费。
且 `map_pipeline_category` 把 `doctitle` 与 `paragraphtitle` 一起映射成 `"title"`,
渲染统一输出 `# `。属于本仓库反复出现的「算出来了但没接进调用链」缺陷类。

> **实测影响有限**:ODL MHS Rust `0.7201` 反而高于 Python `0.7000`——因为 legacy layout
> 压根不发 `doc_title`(class 0 即 `paragraph_title`),Python 把所有标题渲染成 `##` 亏得
> 更多。所以 D6 是设计缺陷,**不是本次回退来源**,优先级低于 D1–D4。

---

### D7:PDF 栅格化 150 DPI,MinerU 为 200 DPI,且不可配置

`runner.rs:1379` 对 PDF 输入硬编码 `raster_dpi = Some(150)`,CLI 无对应开关;
`opensource/MinerU/mineru/utils/pdf_image_tools.py:35` 为 `DEFAULT_PDF_IMAGE_DPI = 200`。
线性分辨率相差 25%,送入 OCR / 表格 / 公式模型的图像尺度与其调优条件不一致。

作用域:**仅影响 ODL(PDF 输入)**;OmniDocBench 输入是图片,由
`materialize_page_source` 直接透传原图,不受此项影响。

---

### D8(既有,未修):Markdown 装配粗糙

`pipeline_comparison_20260825.md` 已记录:uparser 直接把 OCR span 拼接成 Markdown,
没有 MinerU 的重叠清理、块修复、标题合并、段落拆分、语言相关空格与连字符处理。
同权重 A/B 下 Python V2 仍落后原始 MinerU `0.052859`(ODL Overall),并产生 60 个空白页
(原始实现为 2 个)。此项应在 D1–D5 修复、具备公平基线后再处理。

---

## 2. v1 中已被证伪的两条(勿再执行)

### ✗ 「阅读顺序没用真实 LayoutReader 模型,应接入」——方向反了

榜单实测:Python LayoutReader 路径 Order Edit `0.294770`,Rust 几何 XY-cut 路径
`0.288339`(越低越好)。**Rust 的兜底算法在这批语料上反而略优。**
接入 `LayoutReader` 属无收益甚至负收益的改动,**从计划中移除**。若后续要重新评估,必须先
用 A/B 数据证明收益,不能凭「用真模型总比几何启发式好」的直觉推进。

### ✗ 「`discarded` 区域污染 Markdown 输出」——不成立

`discarded` 不在 `TEXT_REGION_LABELS` 中,拿不到 OCR span;`build_blocks` 里
text / html / latex 全为 `None`,`asset_bytes` 也只对 `image`/`chart` 生成,
渲染时四个分支全不命中 → **实际不产生任何输出**。
它仅占用一个 reading-order 槽位并轻微干扰 XY-cut 投影切分,与 D4 的幽灵块同类同量级,
一并在 D4 修复中处理即可,不单列。

---

## 3. 执行方案

### S1 —— 表格专属 OCR 分流(纯 Rust,风险最低)**[已实现]**

**目标**:Table TEDS `27.53 → ~73`,Omni Overall `→ ~75`,ODL Overall `→ ~0.80`。

改 `pipeline_v2.rs::run_workflow`:

1. 把 `table_regions` 的筛选**提前**到 layout 返回之后、OCR 派发之前;
2. 新增第二个 OCR dispatch:table region 的 `label` 改写成 `"text"`(命中
   `TEXT_REGION_LABELS`),`region_id` 保持不变以便回填,发往同一 `ocr_endpoint`;
3. 传给 `table_endpoint` 的 `TableRecognitionInput.ocr_spans` 用**这次**的 spans;
4. 最终 `PageAnalyzeResult.ocr_spans` = 页面级 spans **+** 表格 spans,与
   `page_analyzer.py:173` 的 `ocr.spans + table_ocr.spans` 对齐;
5. 并发结构从两次 `tokio::join!` 改为三路并发(页面 OCR / 表格 OCR / MFR);
   `table_regions` 为空时不发第二个请求。

**验证**:新增 adapter 级 `MockDispatch` 测试,断言表格阶段收到的 `ocr_spans` 来自表格
专属那次调用而非页面级调用(可用不同 mock 响应区分);跑通后重跑 Omni + ODL,
对比 TEDS / TEDS-S / Overall 三项,确认是内容指标收敛而非结构指标被动带动。

**实现记录**:`pipeline_v2.rs::run_workflow` 新增 `table_ocr_regions`(表格区域改标签为
`"text"`)与第三路并发 dispatch(`tokio::join!(ocr, recognized_formulas, table_ocr)`),
`TableRecognitionInput.ocr_spans` 改为消费 `table_ocr.spans`,最终 `ocr_spans` 为两路拼接。
新增 `testing::MockDispatch::dispatch_recording`/`recorded_requests`(仅新增方法,不改动
既有 `dispatch` 路径)让测试能断言"表格 endpoint 实际收到的请求体",而不只是断言"配置的
响应被返回"。`rust_orchestrates_the_complete_v2_ocr_workflow` 测试更新为 seed 两次 OCR
响应并断言表格请求体里的 span 来自表格专属调用。单元测试:359/359(默认 features,不含
沙箱无网络导致失败的 4 个既有测试)。**未跑真实评测**——见 §5。

---

### S2 —— 编排与渲染一致性对齐(纯 Rust,消除剩余回退)**[已实现]**

**目标**:Text Edit 回到 `~0.19`,CDM 回到 `~73`,Omni Overall `→ ~76`(legacy 天花板)。

- **D2**:`map_pipeline_category` 补齐 legacy 词表(`display_formula` → `equation`、
  `inline_formula` → 新增行内公式类目、`figure_title` → `caption`、
  `vision_footnote` → `footnote`、`formula_number` → `text` 或独立类目)。
  同时确认 S3 换 3.4.5 后端后的 PP-DocLayoutV2 词表,避免只对 legacy 打补丁。
- **D3**:渲染层区分行内/块级公式。需要 IR 能表达这一区别——建议由类目承载
  (`equation_inline` vs `equation`),`render::to_markdown` 按类目选择 `$…$` / `$$…$$`。
  注意此改动会影响所有共用该渲染器的协议,需跑 render 快照与 contract 测试。
- **D4**:移植 `merge_layout_and_mfd` 的 IoMin ≥ 0.7 去重(可复用 `geometry.rs` 已有的
  IoU 工具或新增 IoMin),在 `run_workflow` 合并 layout 与 MFD 区域时剔除 layout 版公式区域。
- **D6**:让渲染层消费 `merge_hint::TitleLevel(n)` 输出对应级别的 `#`;
  `map_pipeline_category` 保持 `doc_title`/`paragraph_title` 的层级区分不被抹平。
  优先级低于 D2–D4,可与 S4 合并。

**验证**:每项独立单测;整体重跑 Omni,目标是**逼近但不必超过** Python V2 的 `75.78`
——若 S1+S2 完成后仍显著低于 `75.78`,说明还有未发现的编排差异,不应进入 S3。

**实现记录**:
- D2:`category_map.rs::map_pipeline_category` 接受 legacy 词表全集(`display_formula`/
  `inline_formula`/`figure_title`/`vision_footnote`/`formula_number` 等);`inline_formula`
  映射到新增的独立类目 `"equation_inline"`(不是 `"equation"`)。
- D3:`render::to_markdown` 按 `category == "equation_inline"` 输出 `$...$`,否则维持
  `$$...$$`。
- D4:新增 `pipeline_v2.rs::intersection_over_min_area`(镜像
  `compat.py::intersection_over_min_area`,按最小面积而非并集面积计算重叠比),
  `run_workflow` 用它在 IoMin ≥ 0.7 时剔除 layout 自己检出的重复公式区域。
- D6:`render::to_markdown` 读取 `merge_hint::TitleLevel(n)`(1–6 级)驱动标题深度,
  未设置时退化为单一 `#`,不影响其他协议(只有 `pipeline` 设置过这个 hint)。
- 新增测试:`category_map.rs` 2 个、`render/mod.rs` 2 个、`pipeline_v2.rs` 1 个
  (`duplicate_layout_formula_region_is_dropped_in_favor_of_mfd_detection`)。
  单元测试:359/359(默认 features)。**未跑真实评测**——见 §5。

---

### S3 —— 把 MinerU 3.4.5 拆成纯推理 stage endpoint(**成败关键**)**[已实现,有明确降级范围——见 §5]**

**目标**:解除 legacy 天花板,Omni Overall `→ 85–88`。

**约束落点**(见 §0.5):Python 侧只增加**五个纯推理后端**,不引入任何新的编排;
所有跨 stage 逻辑仍由 `pipeline_v2.rs::run_workflow` 承担。

`mineru345_backend.py` 目前只有整页 `do_parse` finalize 一条路(它同时做了编排和渲染,
违反约束)。需要绕开 `do_parse`,**直接调用 MinerU 内部的单模型封装**,把 3.4.5 的模型
逐个包成 `layout` / `formula_detect` / `ocr` / `formula_recognize` / `table` 五个
`RegisteredBackend`,输出仍走现有 `schemas.py` 的 `Region` / `OcrSpan` / `FormulaSpan` /
`RecognizedTable` 契约(即与 legacy 后端**同契约、可整体替换**),并在
`app.py::create_configured_app` 中于 `profile == "mineru-3.4.5"` 时**替换**而非叠加
`register_pipeline_backends` 的注册。

同时:`pages_analyze` / `documents_analyze` 两个 finalize 后端应标注为
**benchmark-reference-only**(仅用于产出对照基线分),明确不属于产品路径,避免后续再有
代码把编排逻辑往 Python 侧沉。

需要覆盖的模型资产(`mineru345_backend.py:103-111` 已列出校验清单):
`Layout/PP-DocLayoutV2`、`MFR/pp_formulanet_plus_m`(或 `unimernet_hf_small_2503`)、
`OCR/paddleocr_torch/ch_PP-OCRv6_small_{det,rec}_infer`、
`TabCls/paddle_table_cls/PP-LCNet_x1_0_table_cls.onnx`、
`TabRec/SlanetPlus/slanet-plus.onnx`、`TabRec/UnetStructure/unet.onnx`。

**注意点**:
- PP-DocLayoutV2 的类目词表与 legacy 不同,S2 的 D2 修复必须同时覆盖两套(或按
  `profile` 分派不同 mapper)。**词表映射本身属于集成层,归 Rust**,Python 侧只原样返回
  模型的 native label,不做任何归一化。
- 3.4.5 表格链是「分类器 + 有线/无线双模型」,不再是单一 SLANet-plus。Python 侧只负责
  跑分类器与对应识别模型并回填 `classifier_label` / `cells` / `structure_tokens`;
  **依据 `classifier_label` 做差异化装配的逻辑归 Rust**(S4)。
- 3.4.5 的 OCR / 表格阶段在 MinerU 原实现里依赖上游区域裁剪与掩膜语义(参见 legacy 侧
  `recognition_backends.py` 的 50px padding 与公式掩膜)。这些**裁剪/掩膜参数属于模型
  推理的一部分,可留在 Python**;但「哪些区域送 OCR、送哪次 OCR」的决策归 Rust
  ——即 S1 的表格专属 OCR 分流必须继续由 `run_workflow` 驱动,不得下沉到 Python 后端。
- 需要 `transformers>=4.57.3,<5.0.0`(`mineru345_backend.py:29-30`),与 legacy 链的依赖
  可能冲突,注册前应做运行时校验(`validate_transformers_runtime` 已有)。

**验证**:先用 `/v2/models` 确认 5 个 stage 的 `ModelMetadata` 确实换成了 3.4.5 权重,
再跑 Omni;此阶段的对照基线是**本机 MinerU 3.4.5 finalize 的 `85.3391`**,
Rust 分阶段路径应达到同一量级才算 S3 成功。注意该基线是 Python 全包路径,
仅作分数参照,不作架构参照。

**实现记录与 S3 阶段发现的两个新问题**:见 §5(独立成节,因为发现的问题比原计划更深)。

---

### S4 —— 冲击并超越本机最强基线 **[D7 已实现;D8 完成"空白页可观测性"+"语言相关拼接
(CJK 不加空格 / 英文连字符断词重连)"两项,重叠清理/块修复/标题合并/段落拆分仍未做]**

**目标**:Omni Overall **> 88.51**(本机 3.4.5 + PP-Formula),该值同时高于官方 `86.47`。

- **D7**(已实现):PDF 栅格化 DPI 从硬编码 150 改为可配置,默认对齐 MinerU 的 200。
  `runner.rs` 新增 `DEFAULT_RASTER_DPI = 200` 常量,替换掉 `preprocess_plan` 里两处
  硬编码的 `Some(150)`;`ExecutionOptions`/`api::ParseOptions` 新增 `raster_dpi: Option<u16>`
  显式覆盖字段,优先级为「CLI/API 显式指定 > plan 自带值 > `DEFAULT_RASTER_DPI`」;CLI 新增
  `--raster-dpi <N>` 参数,并入 `native` 协议下的"该参数无效"警告列表。新增测试:
  `visual_page_plans_default_to_200_dpi_matching_mineru`(证明 PDF/转换后的结构化文档
  默认走 200 而不是 150)、`raster_dpi_precedence_prefers_override_then_plan_then_default`。
  没有真实 PDF 定级(`pdfium` feature 在此沙箱不可用),因此无法端到端验证"200 DPI 的
  栅格化产物确实变大/更清晰"这一物理效果,只验证了数值传递链路本身是对的。
- **D8**(部分完成):
  - **已做——空白页可观测性**:`pipeline_v2.rs::build_blocks` 新增两类警告:
    (a) 该页 layout 返回零区域;(b) 该页有区域但没有一个产出可渲染内容(无 text/html/
    latex/asset_bytes)。这两种情况此前完全静默,渲染出的空白 Markdown 页和"源文档本身
    就是空白页"在 `warnings` 里无法区分。**未做的是根因修复**——读了
    `pipeline_comparison_20260825.md` 才发现空白页问题在**旧 Python 编排的 V2 就已经
    存在**(60 页对原始 MinerU 的 2 页),说明这不是 `pipeline_v2.rs` 的 Rust 编排引入的
    回归,大概率是 legacy layout 模型在某些页面上把整页误判为 `discarded` 或类似空内容
    类目——这是模型质量问题,理论上应随 S3 换上真实 PP-DocLayoutV2 后自然改善,但**这只是
    推测,本次没有真实数据验证**,新增的警告是为了让下一次真实评测能直接定位到具体是
    哪些页触发了这个失败模式,而不是重新猜。
  - **已做——语言相关空格与连字符处理**(比原计划评估的风险更低,因为规则本身是纯机械、
    有明确判定条件的,不需要真实数据来调阈值):`postprocess.rs::merge_into` 原来无条件用
    `format!("{a} {b}")` 拼接被合并的两行文本,这对英文正确、对中文是个真实缺陷——
    中文行间换行不应该插入空格(例如"…第一条" + "为了…" 会被错误拼成
    "…第一条 为了…",多出一个可见空格)。新增 `join_wrapped_lines`:
    (1) 判定拼接点(前一段末字符/后一段首字符)是否为汉字,是则不加空格直接拼接;
    (2) 判定前一段是否以"字母 + 连字符"结尾且后一段以小写字母开头(典型的英文断词换行,
    如 `"infor-"` + `"mation"`),是则去掉连字符直接拼接成 `"information"`,不是则保留
    连字符原样(避免误伤真正的连字符结尾,如编号 `"A-"` + `"1"`)。新增 4 个测试:
    中文无空格拼接、英文断词重连、"看起来像断词但其实不是"的反例(数字续行)、普通英文
    换行仍保留单空格(回归保护)。全部离线可验证,不依赖真实评测数据——这条不算"盲写
    启发式",因为规则触发条件是字符类别判断,不是需要调参的模糊阈值。
  - **未做——重叠清理 / 块修复 / 标题合并 / 段落拆分**:这是 D8 里仍然需要真实数据才能
    验证的部分——例如"多大的框重叠算重复""标题合并要不要跨页"这类判断没有明确边界,
    调错阈值反而可能引入新回归,留待有真实评测环境时再做。
  - 表格 `classifier_label`(有线/无线)驱动的差异化装配:S3 的
    `Mineru345TableBackend` 已经把 `classifier_label` 真实回填到 `RecognizedTable`,
    但"依据它做差异化 Markdown 装配"这一步在 Rust 侧还没做,原因同上——没有真实表格
    样本可以验证装配差异是否真的有帮助。

---

### 执行顺序与门禁

| 阶段 | 范围 | 语言 | 前置门禁 | 预期 Omni Overall | 状态 |
|---|---|---|---|---:|---|
| **S1** | 表格 OCR 分流 | Rust | — | ~75 | **代码已实现,单测通过,未跑真实评测** |
| **S2** | 词表/行内公式/去重/标题层级 | Rust | S1 通过 | ~76(legacy 天花板) | **代码已实现,单测通过,未跑真实评测** |
| **S3** | 3.4.5 五个纯推理 stage endpoint | Python(仅推理)+ Rust(词表适配) | S2 达到 ≈75.78 | 85–88 | **代码已实现,离线测试通过,未接入真实权重/GPU 验证;表格阶段已知降级(见 §5.3)** |
| **S4·D7** | 栅格化 DPI 可配置,默认对齐 200 | Rust | S3 达到 ≈85 | 影响未知(需真实数据) | **已实现,单测通过** |
| **S4·D8** | 空白页可观测性 | Rust | — | 不直接提分,便于定位 | **已实现,单测通过** |
| **S4·D8** | 语言相关拼接(CJK 不加空格/英文连字符重连) | Rust | — | 影响未知(需真实数据) | **已实现,单测通过(4 个新测试)** |
| **S4·D8(剩余)** | 重叠清理/块修复/标题合并/段落拆分 | Rust | 有真实数据可验证 | **>88.51** | **未开始——阈值需要真实数据调,盲写风险高于价值** |
| ~~接 LayoutReader~~ | ~~—~~ | ~~Python~~ | **取消,数据已证伪** | — | — |

除 S3 外全部在 `uparser-core` 内完成;S3 也严格限定在「模型 → 结构化结果」这一层,
没有把编排逻辑塞进 Python。方案的架构目标(§0.5)——**Python 只剩无状态推理端点,
文档解析的全部智能都在 Rust**——已经落地在代码结构上,但**分数层面的目标(超过官方
`86.47`,冲击 `88.51`)完全没有被验证过**,因为本会话从头到尾都没有可用的 GPU/MinerU
真实运行环境。

**关键判断(仍然成立)**:S1+S2 只是把 Rust 路径修回 Python 路径的水平,**本身无法超过
官方**;S3 是唯一能跨越 10 分模型鸿沟的步骤,是「超过官方」的必要条件;D8 剩余部分决定
能超多少。**下一步不是继续写代码,是找一台有 GPU 和真实 MinerU 3.4.5 权重的机器,把
S1–S4 已完成部分实测一遍**——没有这一步,本文档目前的一切"预期 Omni Overall"都只是
基于旧榜单外推的猜测,不是已验证的结果。

---

## 5. S3 实现记录:两个新发现 + 明确降级范围

S3 落地过程中读了 `opensource/MinerU` 的真实 3.4.5 源码(`model_init.py`/
`pipeline_analyze.py`/`batch_analyze.py`/`pipeline_magic_model.py`/
`pp_doclayoutv2.py`),发现两个原方案没预料到的问题,和一个必须显式收窄范围的表格阶段。

### 5.1 新发现一:PP-DocLayoutV2 没有独立的公式检测(MFD)模型

`model_init.py::MineruPipelineModel.__init__` 只初始化 `layout_model` / `ocr_model` /
`mfr_model` / 表格三件套,**没有任何 MFD 模型**。原因是 PP-DocLayoutV2 是联合
layout+公式检测模型——它自己的原生词表(`pp_doclayoutv2.py::PP_DOCLAYOUT_V2_LABELS`)
本身就包含 `display_formula`/`inline_formula`。legacy 链的 YOLOv8-MFD 是一个独立模型,
这是 legacy 特有的两模型架构,3.4.5 并不沿用。

**处理**:`Mineru345FormulaDetectBackend` 不跑第二个模型,而是共享
`Mineru345LayoutBackend` 的推理结果(按页面图像内容哈希做小容量线程安全缓存)并过滤
`display_formula`/`inline_formula` 两个标签。这样 `pipeline_v2.rs::run_workflow` 里
`layout`/`formula_detect` 两路并发 dispatch(S1 之前就有的设计)不需要改动,3.4.5 profile
下也不会因为这个架构差异而对同一张图跑两次重模型推理。

### 5.2 新发现二:`category_map.rs` 对"确认过的 PP-DocLayoutV2 词表"的注释本身是错的

`PIPELINE_LAYOUT_CATEGORIES` 常量的原注释说"confirmed from `enum_class.py`'s `BlockType`
—— 'Added in pp_doclayout_v2' 子集",但这个 `BlockType` 子集(`abstract`/`doc_title`/
`paragraph_title`/`vertical_text`/`header_image`/`footer_image`/`formula_number`)其实是
MinerU **后处理**用的语义词表,不是模型的原生输出。真正的模型原生词表
(`pp_doclayoutv2.py::PP_DOCLAYOUT_V2_LABELS`)有 25 个标签,与 legacy 词表和
`PIPELINE_LAYOUT_CATEGORIES` 都只有部分重叠——`algorithm`/`aside_text`/`content`/
`number`/`reference`/`reference_content`/`seal` 这几个真实存在于模型输出里的标签,
两套 Rust 侧词表都没有。

**处理**:在 D2 已修复的基础上,`map_pipeline_category` 追加接受这 25 个真实标签,
映射依据是 MinerU 自己的权威转换表——`pipeline_magic_model.py::PP_DOCLAYOUT_V2_LABELS_TO_BLOCK_TYPES`
(直接读源码抄的,不是猜的)。唯一的例外是裸 `"reference"`(list 外框,该转换表里没有
对应条目,推断是被当作纯容器丢弃)——映射到 `"discarded"`,不是编造的。同时更正了
`PIPELINE_LAYOUT_CATEGORIES` 常量原本错误的文档注释。新增测试
`pipeline_category_accepts_the_real_pp_doclayout_v2_label_set` 覆盖全部 25 个标签。

### 5.3 表格阶段的显式降级范围(未完全复刻,已在代码注释中声明)

真实 3.4.5 的表格链比"分类器 + 单模型"复杂得多,读 `batch_analyze.py`/
`unet_table/main.py` 后确认三处未复刻:

1. **有线/无线双跑 + 切换启发式**:`UnetTableModel.predict` 内部无论分类器说什么,
   都会同时跑有线模型,并用单元格数量/文字命中数量与外部传入的 `wireless_html_code`
   比较来决定最终用哪个结果。`Mineru345TableBackend` 只信任分类器的判断,单跑一个模型,
   `wireless_html_code` 传空字符串让切换启发式恒定不生效(已用真实源码验证这个空字符串
   trick 在所有切换条件下都不会误触发)。
2. **表格方向分类**:3.4.5 有独立的 `MineruTableOrientationClsModel`,本实现未接入。
3. **表内公式/图片内联绑定**:`batch_analyze.py::_extract_table_inline_objects` 把落在
   表格区域内的公式/图片转成表格 HTML 里的内联 token,本实现未接入(表格内公式的文字
   仍走 S1 的表格专属 OCR 分流,但不会被渲染成 `<eq>` 内联标记)。

这三处被列为 **S3.1 后续任务**,不是隐藏起来的缺口——文档和代码注释都写清楚了。

### 5.4 已知限制:本会话无法端到端验证 Python 侧代码

本机 `anaconda3` 环境的 `transformers`/`sklearn`/`scipy` 存在 ABI 不兼容
(`numpy.core.multiarray failed to import`),无法真正 `import torch`/`transformers` 之外
的 MinerU 依赖链,更没有 GPU 和真实权重。因此 `mineru345_stage_backends.py` 里所有类都
写成了和这个包里每一个现有 backend 相同的"可注入 loader"形状,并且**全部**用假 loader
做了真正的 `pytest` 离线测试(8 个新测试,全绿),但**这只能验证"编排/装配逻辑"是对的,
不能验证"真的调用 PPDocLayoutV2/PytorchPaddleOCR/UnetTableModel 这几个函数时参数形状
完全匹配"**——这一点只能在有真实环境的机器上跑通才算数。对每个模型类的构造方式和
`predict()` 签名,本次都直接读了 `opensource/MinerU` 的对应源文件逐行确认(不是凭记忆
假设),但没有实际跑起来过。

**做完 S3 之后必须做、但本次没有条件做的事**:在有 GPU 和真实 `mineru.json` 权重配置的
机器上启动 `UPARSER_PIPELINE_PROFILE=mineru-3.4.5` 的服务,先用 `curl .../v2/models`
确认 5 个 stage 都汇报了 3.4.5 的 `ModelMetadata`,再用一张真实页面走一遍
`uparser parse --protocol pipeline`,确认 5 个 stage 都返回非空且合理的结果,然后才
重新跑 Omni/ODL 全量评测。跳过这一步直接相信本次改动一定有效是不负责任的——本次的
真正产出是"结构正确、离线可测的实现 + 三处明确记录的降级点",不是"已验证的分数提升"。

---

## 6. 复现与验证入口

- 本次失败基准产物:
  - `opensource/opendataloader-bench/prediction/uparser-pipeline-v2-rust-cli-staged-20260826/{summary.json,evaluation.json}`
  - `benchmark/OmniDocBench/result/uparser-pipeline-v2-rust-cli-staged-20260826_quick_match_{run_summary,metric_result}.json`
- 复现命令见 `BENCHMARK_REPORT.md` 第 1.2.1 节末尾 `pipeline_v2.rs Rust CLI staged 复现命令`
  代码块(含 `UPARSER_PIPELINE_PROFILE=legacy`、`UPARSER_PIPELINE_DEVICE=cuda`、
  `UPARSER_PIPELINE_MAX_IMAGE_PIXELS=200000000` 三个环境变量;最后一项仅用于可信评测输入,
  生产服务应保留默认 5,000 万像素上限)。
- 每个阶段完成后应同时重跑 ODL 与 Omni 两套评测,**分开报分,不可跨表比较**
  (语料、评测器、指标定义均不同,见 `BENCHMARK_REPORT.md` 开头的对照表)。
