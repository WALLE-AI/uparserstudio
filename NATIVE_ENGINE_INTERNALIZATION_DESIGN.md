# native 引擎内部化技术架构设计与执行方案

> 目标:让 uparser 的 `native` 协议**不再依赖 `opensource/liteparse`**,把所需代码内部化(vendor)进 `uparser/`,并**借鉴 `opensource/pdf-inspector` 的纯 Rust 技术架构**重建 native 引擎。
>
> 状态:设计方案(未实现)。日期:2026-08-04。作者:本次会话。
> 关联文档:`ARCHITECTURE.md`(§8/§13 profiler)、`CLAUDE.md`(P4 native、pdfium 痛点)、`opensource/pdf-inspector/AGENTS.md`。

---

## 1. 目标与非目标

### 1.1 目标
1. **切断 native ↔ liteparse 的编译期依赖**:删除 `uparser-core/Cargo.toml` 里对 `opensource/liteparse/crates/liteparse` 的 path 依赖。
2. **代码内部化**:native 所需的解析能力全部落在 `uparser/` 目录内,不再引用 `opensource/` 下任何工程。
3. **借鉴 pdf-inspector 架构**:采用其"纯 Rust(lopdf)+ 无 OCR + 分类优先 + markdown 优先"的技术路线,顺带**消除 native 对 PDFium 预编译二进制的依赖**(CLAUDE.md 反复记载的构建痛点)。
4. **对外行为保持/提升**:`uparser parse --protocol native` 的 `--format markdown`(liteparse 级质量)与 `--format json`(成行 block IR)对外语义不变或更好。
5. **零波及其他协议**:mineru-vlm / dots.ocr / monkeyocr-v2 / pipeline / paddleocr 不受影响。
6. **基准全面胜出(硬指标,见 §6)**:在 `opensource/opendataloader-bench`(同 harness、同语料、同 evaluator)下,native 的**精度**(Overall / Reading Order / Table / Heading)与**速度**(s/页)**同时超过 liteparse 与 pdf-inspector 两个 baseline**。这要求 native 不能只是"vendor pdf-inspector",必须以其为底座 + uparser 增强层(§4.6)。

### 1.2 非目标(本轮不做)
- **不移除 uparser 的 pdfium**:`ingest::rasterize()`(`ingest.rs:433`,`#[cfg(feature="pdfium")]`)仍需 PDFium 把 PDF 光栅化成图喂给 VLM 协议。pdfium 是**光栅化**用途,与 native 的**文本抽取**用途正交。本方案只让 native 摆脱 pdfium,不动 rasterize。
- 不为 native 引入 OCR(pdf-inspector 本身无 OCR,正合 native 定位;扫描件仍由 uparser profiler/router 交给 VLM)。
- 不改 IR schema(`types.rs::Block/Page`)。

---

## 2. 现状:native 对 liteparse 的依赖面(精确清单)

`native.rs`(433 行)实际消费的 liteparse API 表面:

| 消费点 | liteparse 符号 | 用途 |
|---|---|---|
| 入口 | `LiteParse::new(LiteParseConfig{ocr_enabled:false, output_format})` + `parse_input(PdfInput::Bytes)` | 解析整篇 PDF |
| 结果 | `ParseResult { pages }` | 顶层结果 |
| 页 | `ParsedPage { page_number, page_width, page_height, text, markdown, text_items, projected_lines }` | 页级产物 |
| 行 | `ProjectedLine { text, bbox:Rect, dominant_font_size, spans }` | 空间重建后的成行结构(JSON/IR 路径) |
| span | `TextItem { text, x, y, width, height, font_size, confidence }` | 回退路径 & span 明细 |
| 几何 | `Rect { x, y, width, height }` | bbox |
| 配置 | `config::OutputFormat::Markdown` | markdown 路径 |

依赖声明(`uparser-core/Cargo.toml`):
```toml
native = ["dep:liteparse", "pdfium"]                       # :26  feature 门控
liteparse = { path = ".../opensource/liteparse/crates/liteparse",
              default-features = false, optional = true }  # :54  path 依赖(关 OCR)
pdfium    = { package = "liteparse-pdfium", path = ".../opensource/liteparse/crates/pdfium", optional = true } # :53
```

**结论**:native 是 liteparse(关 OCR)的编译期库依赖 + 433 行 IR 映射。要内部化,必须替换掉"解析地基 + 空间重建 + markdown 组装"这三块能力。

---

## 3. 方案选型

### 方案 A:把 liteparse 代码复制进 uparser
把 `crates/liteparse`(26k 行 Rust)+ `crates/pdfium`/`pdfium-sys` 整体 vendor 进 `uparser/`。
- ✅ 行为 100% 等价,native.rs 几乎不改。
- ❌ 仍绑 **PDFium 预编译二进制**(痛点没解决),违背"借鉴 pdf-inspector"的意图。
- ❌ 需吸收 26k 行含 projection.rs(5,493 行)、OCR(tesseract/oar-ocr)、格式转换等**大量 native 用不到的代码**,维护负担重。
- ❌ 与"借鉴 pdf-inspector 架构"目标冲突。

### 方案 B(推荐):把 pdf-inspector 作为内部 native 引擎 vendor 进来
把 `opensource/pdf-inspector` 的纯 Rust 核心 vendor 成 uparser 内部 crate,native.rs 改调它。
- ✅ **一举同时满足三件事**:去 liteparse 依赖、代码内部化、采用 pdf-inspector 架构。
- ✅ **顺带消除 native 的 PDFium 依赖**(纯 lopdf,零原生二进制,`cargo build` 即得)。
- ✅ pdf-inspector 定位("电子版 PDF <200ms→干净 markdown、agent 优先")**逐字命中 native 的设计目标**;README 宣称在 opendataloader-bench 上 reading-order/table/总分/速度领先。
- ✅ 额外白拿:`classify_pdf_mem`(TextBased/Scanned/Mixed + per-page OCR routing + confidence)、`is_complex_layout`/`pages_with_tables`/`pages_with_columns` → **可直接强化 uparser profiler L2**(现为 `liteparse::is_complex()`)。
- ✅ 许可证 **MIT**(© 2026 Firecrawl),可 vendor,仅需保留署名。
- ⚠️ 需新写 adapter 映射(其 API 是 `process_pdf_mem`→markdown/TextItems,而非 liteparse 的 `parse_input`→projected_lines)。
- ⚠️ 输出与当前 liteparse 版**不逐字节相同**(不同引擎),需要重建 golden 基线。
- ⚠️ **单纯 vendor 只能追平 pdf-inspector,无法满足"超过它"(目标 6)**。故方案 B 必须配套 **uparser 增强层(§4.6)** —— pdf-inspector 作底座,增强层负责在基准上把精度顶过 pdf-inspector,同时速度持平或微优。

### 方案 C:从零手写纯 Rust native 引擎
- ❌ 重复造轮子(PDF 算子/字体/编码/版面/表格是 pdf-inspector 花了 77k 行解决的问题),不现实。

### 选型结论:**方案 B**
用 pdf-inspector 作内部化 native 引擎。下文均按方案 B 展开。

---

## 4. 目标架构设计

### 4.1 顶层形态:新增内部 crate `uparser-native-engine`
```
uparser/
├── crates/
│   ├── uparser-core/            # 现有;native.rs 改调新引擎,删 liteparse 依赖
│   ├── uparser-native-engine/   # 新增:vendor 自 pdf-inspector 的纯 Rust 引擎
│   │   ├── Cargo.toml           #   name = "uparser-native-engine"(内部 crate)
│   │   ├── LICENSE              #   保留 pdf-inspector 的 MIT © Firecrawl(署名合规)
│   │   ├── ATTRIBUTION.md       #   注明源自 firecrawl/pdf-inspector vX.Y.Z + 本地改动清单
│   │   ├── external/bcmaps/     #   随附(1.7M,tounicode.rs 运行期按 CARGO_MANIFEST_DIR 加载)
│   │   └── src/                 #   vendor pdf-inspector/src/,裁剪(见 4.3)
│   ├── uparser-napi/
│   └── uparser-python/
```
**为何独立 crate 而非模块**:①许可证/署名边界清晰;②保留其自带 267+ 单测/73+ 集成测,回归有保障;③与 uparser-core 编译隔离,`native` feature 只在需要时把它拉入;④未来同步上游更容易(diff 一个目录)。

### 4.2 依赖与 feature 改造(`uparser-core/Cargo.toml`)
```toml
# 改造后
native = ["dep:uparser-native-engine"]     # 不再依赖 pdfium!
# 删除: liteparse 的 path 依赖
# 保留: pdfium 仍作为独立 feature 供 ingest::rasterize() 用于 VLM 光栅化
pdfium = ["dep:pdfium"]                     # 维持不变,与 native 解耦
uparser-native-engine = { path = "../uparser-native-engine", optional = true }
```
关键变化:`native` **不再** `pull pdfium`。native 变成纯 lopdf、零原生二进制。

### 4.3 引擎裁剪(vendor 时)
从 pdf-inspector 保留 / 移除:
- **保留**:`extractor/`、`tables/`、`markdown/`、`detector.rs`、`types.rs`、`tounicode.rs`、`text_utils.rs`、`fonts`、`structure_tree.rs`、`reading_order.rs` 等核心。
- **移除/停用**:`src/python.rs`(pyo3 绑定,本地不用,关 `python` feature)、`src/bin/*`(pdf2md/detect-pdf/dump_ops,uparser 有自己 CLI)。
- **feature**:`default = []`,不启 `python`;wasm target 依赖(`include_dir`)可留可裁(uparser 目前不出 wasm)。

### 4.4 native adapter 重写(`uparser-core/src/adapters/native.rs`)
API 对接映射:

| native 需求 | 现(liteparse) | 改(pdf-inspector) |
|---|---|---|
| markdown 路径 | `parse_input(Markdown)`→拼 `page.markdown` | `process_pdf_mem(bytes)` → `PdfProcessResult.markdown`(整篇);或 `extract_pages_markdown_mem` 逐页拼 |
| JSON/IR 路径 | `projected_lines` → 每行一个 `Block` | 引擎的**行级结构**(`types::TextLine`)→ `Block`;无行级则用 `TextItem`(per-span)聚合成行 |
| bbox/字号/spans | `ProjectedLine.bbox/dominant_font_size/spans` | `TextItem{x,y,width,height,font_size,is_bold,is_italic,...}` 组装 |
| 页尺寸 | `page_width/height` | 引擎页信息 |
| 错误 | `map_err → PageError` | 同上,包 `PdfError` |

保留现有分支结构(`run_parse_native`:markdown 走引擎 markdown,json 走 block IR),**不碰 `render::to_markdown` 与其他 adapter**——隔离性沿用上一轮已验证的做法。

### 4.5 顺带增益(可选,建议纳入):强化 profiler
`profiler.rs::profile_l2` 现调 `liteparse::is_complex()`。改用引擎的 `classify_pdf_mem` / `PdfProcessResult` 字段:
- `pdf_type`(TextBased/Scanned/Mixed/ImageBased)+ `confidence` → 更准的 L2 分类。
- `pages_needing_ocr` → 直接支撑 router 的"该页转 VLM"决策。
- `is_complex_layout` / `pages_with_tables` / `pages_with_columns` → 填 `DocumentProfile`。
- 好处:profiler 也一并去掉对 liteparse 的依赖(目前 profile_l2 是 `#[cfg(feature="native")]` 复用 liteparse)。

> 注:此项会改动 profiler 行为与其 golden,建议作为独立阶段(P5)可选推进,不阻塞 native 主线。

### 4.6 uparser 增强层(超越 pdf-inspector 的关键)
底座是 pdf-inspector,增强层全部落在 **native adapter / uparser 侧**(不改引擎核心,便于同步上游),负责在基准上把精度顶过 pdf-inspector。分工:

- **引擎核心(vendored pdf-inspector,尽量不改)**:PDF 算子解析、字体/编码、列检测、三策略表格、markdown 组装。
- **增强层(uparser 新增/复用,可度量)**:
  1. `postprocess::merge_paragraphs_by_geometry`(已存在)——段落合并,利 Reading Order/文本连贯。
  2. `content_normalize`(已存在)——CJK 标点规范化,利文本相似度。
  3. **表格调优**(最大精度杠杆):对引擎三策略阈值按 200 篇 GT 用 TEDS 反馈调参;必要时融合 liteparse 的 ruled-table 思路做 hybrid(注意:这是"借鉴思路"而非"依赖 liteparse")。
  4. **标题分级调优**:按 GT 用 MHS 反馈调 tier 阈值。
  5. **一次解析、两种渲染**:统一 markdown/json 路径避免重复解析(利速度)。

设计原则:增强层与引擎核心解耦,任一增强项都能独立开关并单独度量其对 Overall/子项的贡献(便于消融与防过拟合)。

---

## 5. 依赖 / 边界 / 许可证

| 事项 | 处理 |
|---|---|
| **PDFium 归属** | 保留在 `pdfium` feature,仅供 `ingest::rasterize()`(VLM)。native 与之解耦。 |
| **bcmaps 运行期资源** | pdf-inspector 的 `tounicode.rs` 按 `CARGO_MANIFEST_DIR` 加载 `external/bcmaps`。vendor 到 `uparser-native-engine/external/bcmaps`,保证 `CARGO_MANIFEST_DIR` 指向新 crate 根即可(路径天然正确)。 |
| **许可证** | pdf-inspector = MIT © 2026 Firecrawl。保留 `LICENSE`,加 `ATTRIBUTION.md` 注明来源版本与改动。uparser 顶层若有 license 汇总需登记。 |
| **新增 crates.io 依赖** | lopdf、rayon、ttf-parser、regex、unicode-normalization、once_cell、log、env_logger(全纯 Rust,已在其 Cargo.lock)。评估与 uparser 现有依赖的版本冲突(如 thiserror 2.0、regex 1.x 应兼容)。 |
| **workspace** | 把 `uparser-native-engine` 纳入 `uparser/Cargo.toml` 的 `[workspace.members]`。 |

---

## 6. 基准评审:opendataloader-bench(精度与速度双超越)

### 6.1 harness 概览
`opensource/opendataloader-bench`:**200 篇真实 PDF + markdown ground-truth**,模块化引擎评测。指标与 native 目标高度对齐:

| 指标 | 算法 | 方向 |
|---|---|---|
| Reading Order | NID(归一化编辑距离) | 越高越好 |
| Table | TEDS(树编辑距离相似度) | 越高越好 |
| Heading | MHS(标题层级相似度) | 越高越好 |
| **Overall** | 上三者**等权 fmean**(缺项排除),`evaluator.py:107` | 越高越好 |
| Speed | s/页 | **越低越好** |

接入方式:在 `src/engine_registry.py` 注册引擎名 + 加一个 `src/pdf_parser_<name>.py`(实现 `to_markdown(doc_paths, input_path, output_dir)` 逐篇写 `.md`)。运行:`python src/run.py --engine <name> --force` → `prediction/<name>/evaluation.json`。

### 6.2 现有 baseline(README 公布)与口径陷阱

| 引擎 | Overall | RO | Table | Heading | Speed s/页 |
|---|---|---|---|---|---|
| opendataloader-hybrid(天花板) | 0.907 | 0.934 | 0.928 | 0.821 | 0.463 |
| docling | 0.882 | 0.898 | 0.887 | 0.824 | 0.762 |
| mineru | 0.831 | 0.857 | 0.873 | 0.743 | 5.962 |
| **liteparse(公布口径)** | **0.576** | 0.866 | **0.000** | **0.000** | 1.061 |
| **pdf-inspector** | **不在当前分支**(`edgeparse` 是另一个引擎,非它) | | | | |

**口径陷阱(必须修正)**:bench 的 `pdf_parser_liteparse.py` 写的是 `result.text`(**纯文本**),不是 markdown —— 这正是它 Table/Heading=0 的原因。因此"超过公布版 liteparse"没有意义;必须用**公平口径**(liteparse markdown 输出)重测,native 的胜出才可信。同理 pdf-inspector 当前分支缺席,需自行接入实测,不能引用其 README 的自评。

### 6.3 三方公平重测(前置动作)
在 bench 内新增/修正三个适配器,同台竞技:
- 新增 `pdf_parser_pdf_inspector.py` → `pdf_inspector.process_pdf(path).markdown`
- 新增 `pdf_parser_uparser_native.py` → subprocess `uparser parse --protocol native --format markdown <pdf>`,stdout 写 `.md`
- 修正 `pdf_parser_liteparse.py` → 改用 liteparse **markdown** 输出(公平 baseline;与本项目 native 现状对齐)

三者跑同一 `run.py`、同一 evaluator、同一 200 篇语料,得到可比的 `evaluation.json`。

### 6.4 量化目标(验收硬指标)
在上述同口径下,native 须:
- **精度**:`native.Overall > max(liteparse_md.Overall, pdf_inspector.Overall)`,且 **RO / Table / Heading 三个子项各自不劣于**两 baseline 的对应值;力争逼近 hybrid 天花板(0.907)。
- **速度**:`native.s_per_page ≤ pdf_inspector.s_per_page`,且 `≪ liteparse`(liteparse 1.061,native 现 ~0.03,天然优势)。

### 6.5 native 如何超越"作为底座的" pdf-inspector
以 pdf-inspector 为引擎核心,靠 **§4.6 增强层**拉开差距——精度是主战场,速度求持平/微优:

- **精度**(可度量、可消融):段落合并 + 标点规范化(利 RO/文本);**表格调优是最大杠杆**(liteparse 0.0、pdf-inspector 尚有空间、天花板 0.928)——按 GT 用 TEDS 调三策略阈值,必要时融合 liteparse ruled-table 思路;标题按 MHS 调 tier。
- **速度**:单一 Rust 二进制无 Python 启动开销;**消除 md/json 重复解析**(统一"一次解析两种渲染");必要阶段最小化(native 无 OCR/光栅化)。
- **诚实边界**:与 pdf-inspector 同引擎,速度大概率**持平或微优**,不承诺"大幅超越速度";**精度超越靠增强层**,可控可度量。

### 6.6b PB 首轮实测结果(2026-08-04)

三方接入 opendataloader-bench(200 篇,同 harness/evaluator),实测:

| 引擎 | Overall | RO(nid) | Table(teds) | Heading(mhs) | 速度 s/篇 |
|---|---|---|---|---|---|
| **uparser-native** | **0.8754** | 0.9150 | 0.8141 | 0.7875 | 0.046 |
| pdf-inspector(pdf2md) | 0.8754 | 0.9150 | 0.8141 | 0.7875 | 0.032 |
| liteparse(榜单,text 口径) | 0.576 | 0.866 | 0.000 | 0.000 | 1.061 |
| docling / hybrid(参照) | 0.882 / 0.907 | | | | 0.762 / 0.463 |

**结论**:
- **vs liteparse:精度(0.875 vs 0.576)+ 速度(0.046 vs 1.061)双维度决定性胜出。** 目标达成。
- **vs pdf-inspector:精度逐字节打平**(native 当前直通 `process_pdf_mem().markdown`,与 pdf2md 输出完全相同),速度略慢(CLI 每篇约 +0.014s 固定开销)。native 已达到"最强纯 Rust 引擎"的同等水平,且 >docling 的第 3 名区间。

**为何暂未超过 pdf-inspector(诚实记录)**:
- native 内嵌 pdf-inspector 引擎,markdown 直通 → 天生打平;要超越只能靠 §4.6 增强层或引擎核心调优。
- 实测发现 **MHS 指标层级无关**(`evaluator_heading_level.py`:"treats all heading levels as equivalent",APTED 只比 heading/content tag,不比 `#` 层数)——所以"拍平标题级别"是无效增强(实测 Δoverall 仅 +0.0002,纯噪声),已撤回。
- MHS 的真实杠杆是**标题过检测**(引擎产 280 个标题 vs GT 193 个)与 Table 的 TEDS(0.814→,天花板 0.928),两者都属**引擎核心调优**,会使 vendored 引擎与上游分叉(§6.5 已预警)。
- 速度上,native 与 pdf-inspector 同引擎,native 只多不少(CLI 开销),**结构上无法"更快"** —— §6.5 的"诚实边界"得到实证。

**修正后的目标定位**:native 对 liteparse 是全面超越(已达成);对 pdf-inspector 是**内部化 + 打平**(去外部依赖、去 PDFium、并入 uparser IR/CLI/路由),严格超越需专门的引擎核心调优迭代(§6.6 回路,分叉上游),作为后续可选工作。

### 6.6 GT 驱动调参回路与防过拟合
闭环:改增强层阈值 → `run.py --force` → 读 `evaluation.json` 的 NID/TEDS/MHS → 迭代。**纪律**:从 200 篇中划出 held-out 子集(建议 20%,收尾才验证),防止 native 只在这批语料上刷分;每个增强项单独消融,确认其对 Overall 的净贡献为正。

---

## 7. 分阶段执行计划(带交付物与 Gate)

### P0 — 引擎独立可用性验证(先证可行)
- 交付:把 pdf-inspector clone/复制到 `uparser/crates/uparser-native-engine`,改 `[package].name`,关 `python`/删 bins,纳入 workspace。
- 动作:`cargo build -p uparser-native-engine` + 跑其自带测试。
- **Gate G0**:引擎在本仓库独立编译通过、自带单测全绿;`process_pdf_mem` 能对 `opensource/MinerU/demo/pdfs/demo1.pdf` 产出非空 markdown。

### P1 — native adapter 切换到新引擎(markdown 路径)
- 交付:`native.rs` 的 `native_markdown()` 改调 `uparser_native_engine::process_pdf_mem(...).markdown`;`Cargo.toml` 的 `native` feature 改为依赖新引擎、去掉 liteparse 与 pdfium 连带。
- **Gate G1**:`uparser parse --protocol native --format markdown demo1.pdf` 产出结构化 markdown(标题/段落/表格);与旧 liteparse 版做**质量对比**(字符/行、标题数、表格数),不追求逐字节相同但需同量级或更优。

### P2 — native adapter JSON/IR 路径
- 交付:`parse_document()` 改用引擎的行级/`TextItem` 结构映射为 `Block`(保留 bbox/font_size/spans、成行而非逐词)。
- 决策点:引擎行级结构(`TextLine`)是否 `pub` 可用;不可用则聚合 `TextItem`→行(复用引擎 layout 信号)。
- **Gate G2**:`--format json` 产出成行 `Block`(多词 block 占比 >90%,对齐上一轮 848/878 的水平);IR 字段完整。

### P3 — 删除 liteparse 依赖 + 回归
- 交付:确认 `uparser-core` 不再出现 `liteparse` 符号(profiler 若未迁则暂留 `#[cfg(feature="native")]` 的 is_complex,见 P5);更新 `native.rs` 模块级注释与 `CLAUDE.md` 的 P4 段落。
- **Gate G3**:`grep -r "opensource/liteparse" uparser/crates/uparser-core/Cargo.toml` 为空(native 主线);`cargo build --features native` 成功且**不触发任何 PDFium 下载**;全测试(default & native)绿;clippy `-D warnings`、fmt 干净。

### P4 — golden/快照重建 + 端到端验证
- 交付:重建 native 相关的 golden/insta 快照(引擎更换属预期变更,需 review diff);跑真实文档端到端。
- **Gate G4**:demo1/demo3 端到端产出人工抽检合格;与真实 mineru-vlm 输出交叉核对文本一致性。

### PB — 基准评审接入与超越(与 P1–P4 交织,是本方案的核心验收线)
- 交付:
  1. bench 内三适配器落地(`pdf_parser_uparser_native.py`、`pdf_parser_pdf_inspector.py`、修正 `pdf_parser_liteparse.py` 为 markdown 口径),注册进 `engine_registry.py`。
  2. 三方公平重测,建立同口径基线(§6.3)。
  3. §4.6 增强层逐项落地 + §6.6 GT 调参回路,逐项消融确认净收益。
- **Gate GB(硬性,本方案成败判据)**:同 harness 下
  - `native.Overall > max(liteparse_md.Overall, pdf_inspector.Overall)`,且 RO/Table/Heading 三子项各自不劣于两 baseline;
  - `native.s/page ≤ pdf_inspector.s/page` 且 `≪ liteparse`;
  - 附三方 `evaluation.json` 与 held-out 子集复核结果为证。

### P5(可选)— profiler 迁移到引擎分类器
- 交付:`profile_l2` 改用 `classify_pdf_mem`/`PdfProcessResult`;`DocumentProfile` 填入更丰富信号;移除 profiler 对 liteparse 的最后引用。
- **Gate G5**:`uparser classify demo1.pdf` 输出的 `DocumentProfile` 合理;router 决策不回归。

### P6(可选)— 上游同步机制
- 交付:`ATTRIBUTION.md` 记录 vendor 的 pdf-inspector commit;写一段"如何 rebase 上游更新"的说明。

**建议顺序**:P0→P1→P2→P3→P4 为主线(去 liteparse 达成);**PB 与主线交织**(P1 一出 markdown 即可先测 baseline,增强层随 P2/P4 逐项加、逐项测),是本方案的核心验收线;P5/P6 为增益,可后置。**去 liteparse 依赖(P3)是"必要条件",基准双超越(GB)是"充分条件",两者都达成方算完成。**

---

## 8. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 引擎输出与 liteparse 版不同 | golden 全变、下游观感差异 | 预期内;P4 显式重建基线并人工 review;markdown 质量以 pdf-inspector README 的 bench 领先性兜底 |
| lopdf 对某些畸形 PDF 兼容性不如 PDFium | 少数文档抽取失败 | native 本就是"电子版快速路径";失败时 uparser router 应回退到 VLM 协议(已有机制) |
| 行级结构未公开(需聚合 TextItem) | P2 工作量上升 | 先探 `types::TextLine` 可见性;必要时在 vendor 副本里加 `pub`(属本地改动,记入 ATTRIBUTION) |
| crates 版本冲突 | workspace 编译失败 | P0 即验证;必要时对齐版本或用 `[patch]` |
| bcmaps 路径在被 vendor 后错位 | CID 解码退化、中文乱码 | P0 用含 CID 字体的中文 PDF 专测一版;确认 `CARGO_MANIFEST_DIR` 指向新 crate |
| MIT 署名合规 | 法务 | 保留 LICENSE + ATTRIBUTION,顶层登记 |
| WASM/napi/python 目标 | 未来若 native 要出 wasm | pdf-inspector 原生支持纯 Rust wasm,反而是加分项;本轮不启用 |

---

## 9. 测试与验收标准

- **构建**:`cargo build --workspace`、`cargo build --features native` 均成功,且 `--features native` **不再下载 PDFium**(关键验收点)。
- **测试**:`cargo test --workspace`(default)、`cargo test --features native` 全绿;新引擎自带测试保留并绿。
- **质量**:demo1.pdf 的 `--format markdown` 结构指标(标题≥、表格≥、字符/行同量级)不劣于当前 liteparse 版。
- **IR**:`--format json` 成行 block、bbox/font_size/spans 完整。
- **隔离**:mineru-vlm/dots.ocr 等协议行为不变(`git diff` 证明未触碰其代码;VLM markdown 交叉核对)。
- **静态**:`clippy --all-targets -D warnings`、`fmt --check` 干净。
- **依赖净化**:`uparser-core` 不再引用 `opensource/liteparse`(native 主线;profiler 视 P5)。
- **基准双超越(核心验收,§6.4 / Gate GB)**:opendataloader-bench 同 harness 同口径下——
  - 精度:`native.Overall > max(liteparse_md, pdf_inspector)`,RO/Table/Heading 三子项各自不劣;
  - 速度:`native.s/page ≤ pdf_inspector` 且 `≪ liteparse`;
  - 证据:三方 `evaluation.json` + held-out 子集复核 + 各增强项消融表。

---

## 10. 开放问题

1. **pdfium 是否也要去 liteparse 化?** 当前 `pdfium = liteparse-pdfium`(path 到 liteparse 仓库的 pdfium crate)。本方案范围内 rasterize 仍需它;若要 uparser 完全不引用 `opensource/liteparse`,需把 `liteparse-pdfium`/`-sys` 也 vendor 进 uparser(独立子任务,不阻塞 native)。
2. **profiler L2 迁移(P5)的时机**:是否本轮一并做,取决于是否要彻底清除 uparser-core 对 liteparse 的所有引用。
3. **行级 IR 粒度**:用引擎 `TextLine` 还是聚合 `TextItem`——P2 探明后定。
4. **是否保留 native 的 liteparse 版作对照**:短期可用 `git` 历史;不建议长期双引擎并存。
5. **上游同步策略**:vendor 后与 firecrawl/pdf-inspector 的后续更新如何合并(P6)。

---

## 附:一句话总结
**用 MIT 的 pdf-inspector 作内部 `uparser-native-engine`(纯 lopdf、零 PDFium)作底座,叠加 uparser 增强层(§4.6),替换 native 当前对 liteparse 的库依赖**——一步达成"去 liteparse 依赖 + 代码内部化 + 采纳 pdf-inspector 架构 + 消除 PDFium 构建痛点",并以 **opendataloader-bench 上精度与速度同时超过 liteparse 和 pdf-inspector(Gate GB)** 为最终验收线。去依赖是必要条件,基准双超越是充分条件。
