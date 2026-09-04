# uparser 技术架构流程梳理与冗余模块分析

> 生成日期：2026-09-02
> 分析基准：分支 `feat/architecture-v2`，HEAD `44649fb`（2026-09-01）
> 方法：`uparser-core` 37 个模块 + 14 个 adapter + 2 个 vendored engine 的调用关系全量 grep，
> 并用仓库内已构建的 `uparser/target/release/uparser`（2026-09-01 构建）做行为实测。
> 起因：对 `THREE_MODE_ARCHITECTURE_AND_PLAN.md` 的评审，以及"梳理流程、找出重复冗余模块"的需求。
>
> **状态（2026-09-03）**：本文描述的是 HEAD `44649fb` 的状态。其中 A 类（死代码）、
> B 类（旁路）、E 类（常量/检测点/重复解析）已按
> `ARCHITECTURE_OPTIMIZATION_EXECUTION_PLAN.md` 的 O1/O2/O4 执行完毕并验证，
> Mode N 的能力洼地已按 O3 补齐。C 类（三套 IR/渲染并存）已按 O5 处理**一半**：
> 渲染入口已收敛为一个函数、IR 上升映射（`ascend.rs`）已实现、`document-json`
> 对所有协议开放；但三个 Markdown 实现仍然并存，因为实测显示走单一渲染器会让
> opendataloader-bench 的 Overall 从 0.8753 掉到 0.5663 —— 根因是 native 的
> `Page`/`Block` IR 把所有内容都归为 `text`，标题和表格根本没进 IR。
> 执行结果与门禁数据见该方案文档的「执行状态」与「附录 A」（尤其 A.7）。

---

# Part 0 · 对 `THREE_MODE_ARCHITECTURE_AND_PLAN.md` 的评审结论

## 0.1 该文档已经过时

`git log` 显示该文档只有一次提交：`0912f20 2026-08-19 feat: performance`，此后再未更新。
而 8-21 起有 6 次提交（`3afa584 v2 architeture design`、`b751614 native and cli`、
`98e98bd/4c57b8b/44649fb pipeline v2`）**已经把它 Part II 的大半实现掉了**。
现在把它当"现状说明"读会得出错误结论。

## 0.2 逐条核对（✅ 已实现 / ⚠️ 部分 / ❌ 仍成立）

| 方案条目 | 真实状态 | 证据 |
|---|---|---|
| D1 单一格式枚举、检测一次 | ✅ | `frontend.rs:10` `pub use uparser_document_engine::DocumentFormat`；`PreflightSource` 持有 bytes+digest+detection，全链路只检测一次 |
| D1 不可达组合给明确错误 | ⚠️ | `.rtf --protocol mineru-vlm` 实测已不是 `decode` 错，而是 `{"code":"ingest_failed","message":"required conversion tool \"soffice\" was not found"}`，exit=2；但**没有 `suggest` 字段**，`reachability()` 矩阵全仓库不存在 |
| D2 runner 统一编排 | ✅ | `runner.rs`（1805 行）`analyze → prepare → execute`；`api.rs` 已全量委托，cli/api 三处重复消除 |
| D2 `--mode` 一级概念 | ✅ | `cli.rs:481 resolve_mode`，`--mode auto\|native\|protocol\|pipeline`，`--protocol` 作别名兼容 |
| D2 native 纳入缓存/postprocess | ❌ | `runner.rs:324` `if protocol == "native" { return execute_native(...) }` —— **在算 cache key 之前就 return**；`postprocess_pages` 只在 398/419 两处（scheduler 路径）调用 |
| D3 `generic-vlm` | ✅ | `adapters/generic_vlm.rs`，`protocol_spec` 中 `decode: Markdown / coordinates: FullPage`，CLI 可选 |
| D4 路由三段决策 | ✅ | `router.rs:94 route_with_preference`：候选打分 + `RoutingEnvironment{native, local_ocr, model_protocol, pipeline}` 可行性过滤 + `RoutePreference{Quality,Speed,Cost}`；`pipeline: false` 不自动兜底，`TableDense→pipeline` 已删 |
| D4 决策落 IR | ✅ | 实测 `--protocol auto t.rtf` 输出含非空 `document_profile` + `routed_by` |
| D5 IR 收敛 / 单渲染器 | ❌ | `structured.rs:6 to_parse_result` 仍是有损下降映射（`width_px: 0, height_px: 0`）；`--markdown-source engine\|canonical` 说明两个渲染器并存 |
| D6 成熟度显式化 | ⚠️ | `protocol_spec.rs` 给了 `mode/shape/transport/decode/coordinates/order` 六维自省（比原设计更好），但没有 `validation` 字段，pipeline 也没进 `experimental-protocols` feature |
| R0 缓存键 P0 | ✅ 已修 | `cache.rs:32 ParamFingerprint.execution` + `runner.rs:460 execution_fingerprint`（含 pages/no_postprocess/assets/pipeline/window/concurrency）。实测 `--pages 999` 后普通跑 = 1 页，污染不复现 |
| §3.3「Mode P 无真实服务、契约自拟」 | ❌ 严重过时 | 现为 `pipeline_v2.rs`(2751) + `pipeline_layout/ocr/formula/table`(4400+) + `tensor_wire.rs`：Rust 侧自做预处理与 PP-DocLayoutV2 / PP-OCRv6 CTC / SLANet+ 解码，Python 只做裸 forward。ODL 200 篇 Overall `0.8579 > MinerU 3.4.5 的 0.8568` |
| §3.4「Mode N 不支持扫描件」 | ❌ 过时 | 新增 `adapters/local_tesseract.rs` + `runner.rs:736 hybrid_ocr_request` / `788 apply_hybrid_ocr`；router 里 `tesseract` 是独立候选 |

**结论**：Part I 的现状描述有一半不成立，Part II 的 D1/D2(半)/D3/D4/D6(半) 已落地。
该文档应重写为「D5 收敛 + 剩余缺口」，或标注失效日期。
设计质量本身是好的——六个决策每条都对准一个可复现的具体缺陷；问题只在于文档没随实现更新，
现在它同时是"过时的现状"和"已完成的计划"，两种用法都危险。

---

# Part 1 · 当前真实的主干流程

只有一条主干，但有 **两条旁路** 绕开它：

```
                                    ┌─ 旁路①: bin/uparser-native.rs (独立 binary，276 行)
                                    ├─ 旁路②: cli.rs:583 native_markdown_fast_path (9 个条件)
                                    ↓
文件 bytes
 │
 ├─ frontend::PreflightSource::new          frontend.rs:241   ← 唯一检测点，产 bytes+sha256+format
 │
 ├─ runner::analyze_inner                   runner.rs:202
 │    ├─ Pdf        → uparser_native_engine::process_pdf_mem()  ← 无论后面走哪个模式，都先整份解析
 │    │                + profiler::profile_l2_result(&artifact)
 │    ├─ 结构化 12 类 → document_engine::parse_document()        ← 同上，先整份解析
 │    └─ 其它        → profiler::profile_l1(format)
 │    产出 AnalysisReport{ profile, artifacts }   ← artifacts 被 native 复用，被 VLM/pipeline 丢弃
 │
 ├─ runner::prepare                         runner.rs:245
 │    ├─ 显式协议 → explicit_route()          runner.rs:1304  ← 可行性断言（native 拒绝 Scanned/ImageOnly）
 │    ├─ auto     → router::route_with_preference()  router.rs:94  ← 4 候选打分+环境过滤+偏好
 │    └─ preprocess_plan()                   runner.rs:1370  ← 决定 raster_dpi / 是否需要 soffice 转换
 │
 └─ runner::execute_with_hooks              runner.rs:307
      ├─ protocol=="native" → execute_native()   runner.rs:324  ← 在 cache 之前 return
      │     ├─ Structured → structured::to_parse_result()（有损）+ document/engine markdown
      │     └─ Pdf → native::parse_pdf_artifact() + [pdfium] apply_hybrid_ocr(tesseract) + 图片落盘
      └─ 其余 → cache::get → Registry::build → materialize_page_source
                 → scheduler::run_with_progress → adapter.parse_page
                      └─ shape_executor::{chat_stage, rest_stage, collect_indexed}
                 → postprocess_pages → assets → cache::put
```

**这张图本身暴露三个结构性问题**：

1. `analyze` 无条件做一次完整解析 —— 走 VLM/pipeline 时这份 artifact 被直接丢弃，纯浪费；
2. `execute_native` 在 cache 之前 return —— Mode N 永远无缓存、无 postprocess（含 CJK 标点规范化）；
3. 两条旁路完全不走这张图 —— 语义与主干不一致（见 Part 2 B 类实测）。

## 1.1 模式与协议的实际枚举

`protocol_spec.rs:6 ModeKind` 有 4 个值，`--mode` 也有 4 个值，与文档的"三模式"说法已对不齐：

| ModeKind | 协议 | shape | transport |
|---|---|---|---|
| `Native` | `native` | `native_document` | `in_process` |
| `Native` | `tesseract` | `one_shot_page` | `in_process` |
| `ModelProtocol` | `mineru-vlm` | `layout_then_recognize` | `open_ai_chat_completions` |
| `ModelProtocol` | `dots-ocr` | `one_shot_page` | `open_ai_chat_completions` |
| `ModelProtocol` | `generic-vlm` | `one_shot_page` | `open_ai_chat_completions` |
| `ModelProtocol` | `monkeyocr-v2` | `layout_then_recognize` | `open_ai_chat_completions` |
| `ModelProtocol` | `paddleocr` | `structured_service` | `paddle_ocr_service` |
| `ModelProtocol` | `paddlex-structure` | `structured_service` | `paddle_ocr_service` |
| `Pipeline` | `pipeline` | `stage_graph` | `pipeline_stages` |
| `Test` | `mock` | `mock` | `none` |

实测成绩（`BENCHMARK_REPORT.md`，opendataloader-bench 200 篇）：
native `0.8754@0.051s` / auto `0.8920@0.137s`（156 篇 native + 44 篇 VLM）/
mineru-vlm `0.9240@0.621s` / pipeline v2 `0.8579@1.286s`。

## 1.2 快速路径（fast path）是什么

`cli.rs:583` 的条件分支 + `cli.rs:970 native_markdown_fast_path()`。
它是**唯一一条不建 tokio runtime、不进 runner、不路由、不缓存、不产 IR** 的通路，
直接 `process_pdf_mem(bytes) → artifact.markdown` 打印。实测 **28 ms**（Economist 单页 PDF）。

触发需同时满足 9 个条件：
`--protocol native` ∧ `--format markdown` ∧ `--markdown-source engine` ∧ `--no-cache` ∧
`!--stream` ∧ `!--no-postprocess` ∧ 无 `--pages` ∧ 无 `--assets-dir` ∧ `--no-assets` ∧
window/concurrency 为默认值。

自带一个退回守卫（`cli.rs:979-996`）：当 `pdfium` 已编译且检出
`OCR_REASON_SUSPECTED_GARBLED_TEXT` / `SCANNED` 或 `looks_like_gbk_utf8_mojibake` 时返回
`Ok(None)`，让流程走回 runner 做 hybrid OCR。

---

# Part 2 · 冗余清单（按成因归类，每条都有证据）

## A 类：阶段化开发的地层沉积 —— 新实现上线了，旧的没删

| 模块 | 行数 | 证据：生产不可达 | 为什么当初需要 |
|---|---:|---|---|
| `adapters/pipeline.rs`（Pipeline V1） | 638 | `Registry::register("pipeline", ...)` 只构造 `pipeline_v2::PipelineV2Adapter`（`adapters/mod.rs:469-470`）。`PipelineAdapter` 除自身测试外无构造点 | P5 的四阶段 REST 契约。V2 改成"Rust 拥有全部预/后处理，Python 只做裸 forward"后，V1 整套契约作废 |
| `adapters/onnx_table.rs` | 126 | 唯一调用点是 `pipeline.rs:436` | P5 的 `StageBackend::Local`。V2 明确 `local is rejected`（见 `--table-backend` 帮助文本） |
| `adapters/pipeline_serving.rs` | 123 | 只剩 `StageImage` 被 `paddleocr.rs:36` 引用，其余 V1 专用 | 同上 |
| `pipeline-local-table` feature + `ort` 依赖 | — | `Cargo.toml:44`，只服务 `onnx_table.rs`；注释自承本机 glibc 2.35 链接不通 | 同上 |
| `ingest::ingest_document` + `structured_bypass` | ~150 | 全仓库除 `ingest.rs` 自身测试外**零调用者**（已 grep 确认）。结构化直读现由 document-engine 的 9 个 frontend 承担 | P7 的 XLSX/CSV 短路。document-engine 覆盖同一职责且更完整（16 变体 vs 2） |
| `profiler::profile_l2`（async、吃 bytes 那个） | ~60 | 生产用 `profile_l2_result(&artifact)`；`profile_l2` 只有测试调用（`profiler.rs:989/1028/1044`） | 早期 profiler 独立于 runner 时需自己再解析一次 PDF；runner 统一编排后 artifact 已在手 |
| `ingest::detect_format` | 3 | 纯转发 `uparser_document_engine::detect_format`，已无生产调用者 | 格式枚举统一前的兼容层 |

**小计 ≈ 1100 行 + 1 个 feature + 1 个重量级可选依赖（`ort`）。**

## B 类：为 benchmark 造的旁路 —— 分数需求倒逼出的第二/第三实现

| 旁路 | 证据 | 为什么当初需要 | 代价 |
|---|---|---|---|
| `cli.rs:583` 快速路径 | 9 个触发条件见 §1.2 | `benchmark/compare_competitors.py:47` 正是这套参数：要和 `pdf2md`/`lit` 这类单进程转换器比冷启动，不能带 tokio runtime、路由、IR | **实测：`--no-cache` 会把"报错"变成"成功"**（见 §2.1） |
| `src/bin/uparser-native.rs` | 独立 `[[bin]]`，`required-features=["native"]`（`Cargo.toml:15-18`） | 更早一版的"最小转换器"，同样为竞品对比 | 三样东西各抄一份：PDF→markdown 逻辑（与快速路径近乎逐行相同）、`tesseract_pdf()` 300dpi 光栅+子进程 OCR（`adapters/local_tesseract.rs` 已有）、`find_tesseract()`（`local_tesseract::executable_path` 已有）。而 `scripts/package-windows.ps1:20` 只发布 `uparser.exe`，`compare_competitors.py` 也已改用主 binary —— **该 binary 现在只有自己的测试在用** |

**小计 ≈ 370 行，且是唯一一处"性能开关改变语义"的地方。**

### 2.1 实测：`--no-cache` 改变语义（image-only PDF）

```
$ uparser parse --protocol native --format markdown --no-assets            X.pdf
error: protocol native cannot process Pdf: no reliable native text or source semantics

$ uparser parse --protocol native --format markdown --no-assets --no-cache X.pdf
# magazine_TheEconomist.2023.12.23_page_052

[Image-only PDF: OCR required]
```

成因：`runner.rs:1325 explicit_route` 对 `SourceQuality::{Scanned,ImageOnly,Unknown}` 显式拒绝
native，而快速路径**根本不调 router**；它自己的守卫又要求 `!artifact.positioned_items.is_empty()`，
image-only PDF 恰好为空，守卫结构上覆盖不到，于是落到 `cli.rs:1004` 的占位分支输出
`[Image-only PDF: OCR required]` 并 exit 0。

**一个纯性能开关不应决定成功还是失败。**

### 2.2 顺带发现：PDF 标题元数据未解码即输出

对 `bench/data/2、JGJ80-2016_建筑施工高处作业安全技术规范.pdf`，快速路径输出：

```
# <4A474A2038302D3230313620BDA8D6FECAA9B9A4B8DFB4A6D7F7D2B5B0B2C8ABBCBCCAF5B9E6B7B62E706466>
```

`uparser-native-engine/src/detector.rs:1837 get_document_title` 只处理 UTF-16BE BOM，
其余一律 `from_utf8_lossy` —— GBK 编码 / 十六进制字符串标题原样漏进 Markdown 首行。

## C 类：三个世界并存 —— 最贵、也最难拆的一类

core 有一套 IR/渲染，`document-engine` 有一套，`native-engine`（vendored 自 pdf-inspector）又有一套。
同一件事被实现了 2–3 次：

| 职责 | 实现 1 | 实现 2 | 实现 3 |
|---|---|---|---|
| Markdown 渲染 | `render/mod.rs::to_markdown`（383 行，从 Block IR） | `document_engine::render::markdown`（从 CanonicalDocument） | `native_engine` 自带 markdown（`markdown/` 2822+2463+1446 行） |
| 表格 → HTML | `otsl.rs::to_html`（OTSL 语法） | `pipeline_table.rs:1123/1148`（SLANet + wired，自带 `escape_html:1108`） | `native_engine/tables/format.rs`（1471 行，出 Markdown 表） |
| 阅读顺序 | `reading_order.rs`（XY-cut，3 个 adapter 调用） | `native_engine/extractor/reading_order.rs`（591 行） | 各 VLM 协议模型自带 order |
| 段落后处理 | `postprocess.rs` + `content_normalize.rs` | `native_engine/markdown/postprocess.rs`（1446 行） | — |
| 语义 IR | `types::{Page,Block,ParseResult}` | `document_engine::CanonicalDocument` | `native_engine::PdfProcessResult` |

**成因**：`native-engine` 是整仓库 vendored 进来的（见 `NATIVE_ENGINE_INTERNALIZATION_DESIGN.md`，
目的是甩掉 liteparse 和 PDFium 二进制），它是个**完整闭环**，从解析一路做到 Markdown；
`document-engine` 是自研多格式前端，同样闭环。core 想统一就必须做降维映射 —— 于是有了
`structured.rs:6 to_parse_result`，一个明确有损的下降映射（`width_px: 0, height_px: 0`，list→text）。

**代价**：`cli.rs:856-874` 有一个四路 if/else 决定用哪个渲染器；
`--markdown-source engine|canonical` 这个 flag 的存在本身就是"我们有两个渲染器且不敢切默认"的自白。

## D 类：看起来重复、其实必须保留（**不要动**）

- `category_map.rs` 的 4 个 `map_*_category` —— 4 个模型的原生词表本就不同（已共享 `normalize_key`）。
- `imaging.rs` 的 `hard_resize` / `smart_resize` / `resize_by_pixel_bounds` ——
  分别是 MinerU / Qwen2.5-VL / MonkeyOCR 的原生 resize 策略，数值必须逐位一致。
- `output_parse.rs` 的三套语法、`geometry.rs` 的三套坐标映射 —— 同理。
- `transport::dispatch` vs `dispatch_rest` —— chat-completions 与自定义 REST 是两种线协议，
  已共用 `post_with_retry` 重试骨架。

**判据**：凡是"某个上游模型 wire 契约"的镜像，重复是正确的；
凡是"我们自己的编排/表达"，重复就是债。

## E 类：常量与调用点散落

- **光栅化 DPI 有 4 个值**：`DEFAULT_RASTER_DPI=200`（`runner.rs:24`）、`300.0`（`runner.rs:804`
  hybrid OCR）、`150.0`（`runner.rs:1123` native 图片裁剪）、`300.0`（`bin/uparser-native.rs`）。
  三个 magic number 无常量、无注释说明为何不同。
- **`detect_format` 仍有 3 个调用点**：`frontend.rs:241`（权威）、`api.rs:163`、
  `cli.rs:975`（快速路径内，对同一份 bytes 的第二次检测）。类型已统一（`ingest::DocumentFormat`
  只是 re-export），但调用点没收敛。
- **结构化文档可能被解析两次**：`runner.rs:718 document_options_require_reparse` ——
  任何非默认的 `--no-notes` / `--headers-footers` / `--max-input-mib` 都会触发第二次完整解析，
  因为 `analyze` 阶段是用默认 options 解析的。

---

# Part 3 · 为什么会长成这样（三条结构性成因）

理解成因比列清单重要，否则清理完还会再长出来。

**1. `pub mod` 关闭了编译器的死代码检测。**
`lib.rs` 把 37 个模块全部 `pub`，`adapters/mod.rs` 把 14 个 adapter 全部 `pub`。
于是 `PipelineAdapter`、`ingest_document`、`profile_l2` 这些**零生产调用者**的东西，
`cargo build` 和 `clippy -D warnings` 都不会吭一声 —— 它们还各自带着 4 / 22 / 13 个绿色测试，
看起来"健康"。**这是 A 类冗余能存活的唯一原因：没有任何自动信号告诉你它死了。**

**2. 强验证文化 + 增量交付 = 只加不减。**
本项目的做法是"每批改动都要有真实端点/真实文档验证"，非常好，但副作用是：
旧实现有完整测试和历史验证记录，删它需要重新论证"没人用"，成本高于留着。
于是 P5 的 pipeline v1 在 v2 上线后原地封存。

**3. benchmark 是一等公民，且评的是"进程级墙钟"。**
`compare_competitors.py` 比的是冷启动到出 Markdown 的总时间，对手是单进程 Rust 转换器。
统一 runner（tokio runtime + 路由 + IR + 缓存）在这个口径下天然吃亏，于是为分数造了旁路。
**B 类冗余的根源不是懒，是评测口径与产品架构的目标冲突。**

---

# Part 4 · 建议的拆解顺序

按 **收益/风险比** 排，前两步基本零风险。

## 第 1 步：删 A 类死代码（约 1100 行，风险 ≈ 0）

删除清单：

- `adapters/pipeline.rs`（638）
- `adapters/onnx_table.rs`（126）
- `adapters/pipeline_serving.rs`（123；把 `StageImage` 挪进 `shape_executor.rs`）
- `ingest::{ingest_document, structured_bypass, structured_bypass_xlsx, structured_bypass_csv, detect_format}`（~150）
- `profiler::{profile_l2, profile_structured}`（~60）
- `Cargo.toml` 的 `pipeline-local-table` feature 与 `ort` 依赖

连带删掉它们的测试（22 + 13 + 4 + 2 + 4 个）。这些测试当前是负资产：
它们让死代码看起来有人维护。

**同时加防复发机制**：把 `lib.rs` 的模块尽量降为 `pub(crate)`，
只保留 `api` / `types` / `frontend` / `runner` / `protocol_spec` 对外
（binding crate 实际只用这几个）。这样下一个模块死掉时编译器会直接报 warning。

## 第 2 步：收敛 B 类旁路（约 370 行，风险低）

- **删 `src/bin/uparser-native.rs`**：无发布、无外部调用者，
  且它自带的 tesseract 实现会与 `local_tesseract.rs` 持续漂移。
- **把快速路径从"CLI 的 if 分支"改成"runner 内部的短路优化"**：
  仍先跑 `analyze`（PDF artifact 反正要产），但在
  `protocol==native ∧ format==markdown ∧ markdown_source==engine` 时跳过
  IR 构建 / 缓存 / 资产环节直接返回 `artifact.markdown`。
  这样语义唯一（image-only 仍被 `explicit_route` 正确拒绝），省下的仍是大头。
  若实测 tokio runtime 启动是瓶颈，用 `new_current_thread`
  （`cli.rs:630` 对 native 已经这么做了）而不是整条绕开。
- 顺手修 `detector.rs:1837 get_document_title` 的非 UTF-16 解码（见 §2.2）。

## 第 3 步：补齐 Mode N 的能力洼地

把 `runner.rs:324` 的 `if protocol == "native" { return ... }` 从"cache 之前"挪到"cache 之后"，
让 native 也享受内容哈希缓存 + `postprocess_pages`。
现在 Mode N 之所以看起来像"另一个产品"（无缓存、无标点规范化、`--pages`/窗口/并发无效），
根因就是这一行提前 return。

## 第 4 步：C 类（大工程，单独立项）

不要试图合并三套 IR 的**存储表示**（原方案 §8 的判断正确）。可行的最小切口：

1. 先把 `structured.rs` 的有损**下降**映射反过来，做 `Block[] → CanonicalDocument` 的**上升**映射；
2. 让所有 Markdown 都由 `document_engine::render` 产出，
   `native-engine` 自带 markdown 降级为 `--markdown-source engine` 的可选项；
3. 有黄金测试保护（0.8754 / 0.9240 两个基准不回退）后再切默认。

在 1–3 步完成前不要碰这一块 —— 它需要 benchmark 保护，而 1–3 步会让 benchmark 路径本身变干净。

## 第 5 步：E 类常量与调用点收敛

- 四个 DPI 各自建常量并写清理由
  （`RASTER_DPI_MODEL=200` / `RASTER_DPI_OCR=300` / `RASTER_DPI_ASSET_CROP=150`）。
- `analyze` 增加"VLM/pipeline 路径不需要 artifact"的短路，
  避免 `--protocol mineru-vlm` 时白跑一遍完整 lopdf 解析。
- 把用户 `ParseOptions` 前移到 `analyze`，消除 `document_options_require_reparse`
  导致的结构化文档二次解析。

---

# Part 5 · 一句话总结

真正的冗余集中在两处：

1. **被 V2 取代但没删的 pipeline V1 一族（A 类）**；
2. **为 benchmark 冷启动分数绕开统一 runner 的两条 native 旁路（B 类）**。

合计约 **1500 行**。它们能长期存活是因为 `pub mod` 让编译器看不见死代码。

而 `category_map` / `imaging` / `output_parse` 那种"看起来最重复"的部分，
恰恰是必须保留的协议保真层（D 类）。

三套 IR / 渲染器（C 类）是 vendored 两个完整引擎的必然结果，是笔真实的债，
但要等前两步清完、benchmark 路径统一后再动。
