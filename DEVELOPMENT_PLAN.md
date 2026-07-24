# uparserstudio 研发迭代计划（Task 级）

> 依据：`ARCHITECTURE.md` v0.9　|　目标：把技术架构拆解为可分配、可验收的研发任务，按迭代阶段推进，每个阶段有明确的**验证门（Gate）**，门不过不进入下一阶段。
>
> 约定：
> - **任务 ID**：`T-<阶段>.<序号>`；**依赖**列出前置任务；**规模** S(≤2人日)/M(3-5人日)/L(1-2人周)；**验收**为可客观判定的完成标准。
> - **Gate**：阶段末的架构级验证点，对应 `ARCHITECTURE.md` 里"这是关键验证点"的地方，Gate 失败要回到设计而非硬推。
> - **Python 原型并行轨**（§9.4）：协议逆向（prompt/输出格式复刻）先用 Python 对真实 vLLM 服务跑通再移植 Rust；见附录 A。
> - 语言/库选型、模块划分见 `ARCHITECTURE.md` §9；各协议细节见 §3/§10/§11；各引擎源码级契约见 `mineru_report.md`/`monkeyocrv2_report.md`/`dots_ocr_report.md`/`liteparse_report.md`。

---

## 0. 阶段总览

| 阶段 | 主题 | 核心产出 | Gate（通过条件） | 依赖阶段 |
|---|---|---|---|---|
| P0 | 骨架 + IR + 执行模型 | 可编译的 crate 骨架、增强 IR、scheduler、transport、CLI 契约 | 空跑一页假数据能走通 ingest→scheduler→(mock adapter)→render，退出码/stdout-stderr 契约成立 | — |
| P1 | **首个协议闭环（mineru-vlm 两阶段）+ 共享层 + 共享解析/容错**【最关键，前置 G1】 | `adapters/mineru_vlm.rs`、`imaging.rs`、`geometry.rs`、`otsl.rs`、`formula_repair.rs`、`robustness.rs`、`output_parse.rs`(custom_token)、`postprocess.rs` | **`parse_page` 多轮编排承载两阶段协议成立**（最难关前置）；真实 vLLM 上 MinerU 端到端出正确 Markdown；共享层建成 | P0 |
| P2 | 第二协议（dots.ocr 单轮）—— 验证共享层"协议无关性" | `adapters/dots_ocr.rs`、`output_parse.rs`(strict_json 容错链) | **同一套共享层（geometry/category_map/postprocess/render）在不改动前提下同时正确处理 mineru-vlm 与 dots.ocr 两种 IR**——证明共享层非 MinerU 专属 | P1 |
| P3 | MonkeyOCR-v2 + 共享模块复用验证 | `adapters/monkeyocr.rs` | 第三个协议接入**不改动**共享层任何一行；OTSL/formula/robustness 三模块被两个协议共用 | P2 |
| P4 | native 零模型协议 | `adapters/native.rs`（内嵌 liteparse）、Cargo feature 化 | 数字原生 PDF 走 native 零模型出正确 Markdown；`--no-default-features` 可去掉 liteparse | P1 |
| P5 | pipeline 协议 + 逐 stage 后端 | `adapters/pipeline.rs`、`ModelStage`/`StageBackend`、Pipeline Model Serving 契约 | table stage 走本地 `ort`、layout/ocr/formula 走 Remote，客户端 CPU 占用显著低于全本地 | P2 |
| P6 | PaddleOCR + 阅读顺序回退 | `adapters/paddleocr.rs`、`reading_order.rs` | 无原生阅读顺序的协议经几何回退得到合理顺序；polygon 几何被正确处理 | P2 |
| P7 | 多格式接入 | `ingest.rs` 的 detect/normalize/structured_bypass | PPT/Word 转 PDF 后走任一协议；xlsx 结构化旁路直出，不经模型 | P1 |
| P8 | Profiler + Router + auto | `profiler.rs`、`router.rs`、`uparser classify`/`--protocol auto` | L1/L2 免费启发式正确把"数字原生长文本→native、复杂→VLM"分流；DocumentProfile 随结果输出 | P4, P7 |
| P9 | 缓存 + 流式 + doctor + 契约测试 | `cache.rs`、流式输出、`uparser doctor`、回归测试集 | 同文档二次调用命中缓存；大文档窗口化不 OOM；契约测试覆盖部分失败/截断/降级 | P2, P5 |
| P10 | 语言绑定 + 发布 | `uparser-napi`、`uparser-python`、打包 | Node/Python 包能调用同一 core，IR 序列化一致 | P9 |

**关键路径**：P0 → P1 → P2 是架构成立性的主干。**采用"最难关先行"策略——把两阶段协议 mineru-vlm 作为首个协议（G1），第一时间验证 `parse_page` 能否承载两阶段编排**（这是本次架构 v0.9 重构的核心风险点，早暴露早止损）；随后用单轮的 dots.ocr 作为第二协议（G2）反向验证"共享层不是 MinerU 专属"。P4/P7 可与 P2 并行。P3/P5/P6 在 G2 通过后扇出。P8 依赖 P4+P7。

---

## 1. 依赖关系（简化图）

```
P0(骨架/IR/scheduler/transport/CLI契约)
 └─> P1(mineru-vlm两阶段+共享层+共享解析)[G1:两阶段编排成立]
      ├─> P2(dots.ocr单轮,验证共享层协议无关)[G2:跨协议成立] ──┬─> P3(monkeyocr复用验证)
      │                                                        ├─> P5(pipeline+stage后端)
      │                                                        ├─> P6(paddleocr+reading_order)
      │                                                        └─> P9(缓存/流式/doctor/测试)
      ├─> P4(native零模型) ─────────────────────────┐
      └─> P7(多格式接入) ───────────────────────────┴─> P8(Profiler+Router+auto)
 P9 ──> P10(绑定/发布)
```
> 注：`cache.rs`(T-9.1) 建议 P1 之后即并行开工（收益高）。P4/P7 依赖 P1 建成的共享层，可与 P2 并行。

---

## 2. 详细任务分解

### P0 — 骨架 + 统一 IR + 执行模型
> 目标：把 `ARCHITECTURE.md` §2/§2.2/§5/§6.1/§9.2 落成可编译骨架，为后续协议接入提供地基。

| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-0.1 | Cargo workspace 与 crate 骨架（`uparser-core` + 占位 `-napi`/`-python`），CI（fmt/clippy/test） | — | S | `cargo build`/`clippy`/`test` 绿；目录结构对齐 §9.2 |
| T-0.2 | 统一 IR 类型（§5）：`Block`(含 `spans`/`merge_hint`/`geom`/`geom_frame`/`source`)、`Page`、`ParseResult`(含 `page_errors`/`capability_notes`/`routed_by`)、`Geometry`、`CoordFrame`、枚举 | T-0.1 | M | serde 往返测试通过；`cargo doc` 字段注释齐全 |
| T-0.3 | `ProtocolAdapter` trait（§2.0，`#[async_trait]`）+ `ParseCtx`（dispatch/crop/permit/cached_or 的接口签名，先给 stub 实现）+ `registry` | T-0.2 | M | `Box<dyn ProtocolAdapter>` 可注册可取用；能写一个 mock adapter |
| T-0.4 | `transport.rs`：reqwest+tokio 的 OpenAI 兼容 chat-completions 客户端（图像 base64 data URL、采样参数透传、重试+退避+超时、`asyncio.Semaphore` 等价的并发限流） | T-0.1 | L | 对着 mock HTTP server 的并发/重试/超时单测通过 |
| T-0.5 | `scheduler.rs`（§2.2）：处理窗口分批、文档级并发预算令牌、逐窗口推进+页图及时释放、单页失败隔离为 `PageError` | T-0.3 | L | 用 mock adapter 跑 100 页假文档，峰值内存 ~O(窗口) 而非 O(总页)；单页抛错不影响其余页 |
| T-0.6 | `ingest.rs::rasterize()`：pdfium 栅格化（复用 liteparse 的 `pdfium` crate），DPI 可配、记录原始尺寸 | T-0.1 | M | 一份多页 PDF 逐页得到 `RenderedPage`（图+尺寸） |
| T-0.7 | CLI 骨架（clap）+ **Agent-first 契约**（§6.1）：stdout=结果/stderr=日志、语义化退出码(0/1/2/3/4)、`--format`、结构化错误对象、`--stream` 占位 | T-0.2 | M | `uparser parse --help` 成立；错误场景返回正确退出码与 JSON error 对象 |
| T-0.8 | `render/`：从 IR 渲染 Markdown / JSON / content-list（先不含表格 HTML 内嵌等高级项） | T-0.2 | M | 给定手写 IR，产出三种格式，快照测试固定 |
| T-0.9 | **测试基础设施**（§5.3）：`MockDispatch` 派发替身（按请求返回预置响应，供两阶段编排离线测试）+ `tests/fixtures/{raw,docs,golden}/` 目录骨架 + `insta`/`wiremock`/`proptest`/`cargo-llvm-cov` 接入 + CI 门（§5.6） | T-0.3,T-0.4 | M | mock adapter 能用 `MockDispatch` 离线跑通；CI 跑 fmt/clippy/test/覆盖率；快照机制可用 |

**Gate G0**：`uparser parse fake.pdf --format json` 用 mock adapter 走通全链路（ingest→scheduler→adapter→geometry→render→stdout），退出码与流分离契约成立；**测试基础设施（MockDispatch/fixtures/CI 门）就位，后续每个 adapter 可立即写离线单测**。

---

### P1 — 首个协议闭环（mineru-vlm 两阶段）+ 共享层 + 共享解析/容错【最关键阶段，最难关先行】
> 目标：**把最难的两阶段协议 mineru-vlm 作为第一个接入的协议**，第一时间验证 v0.9 重构的核心假设——`parse_page` 单接口能否承载"版面→逐区块"两阶段编排；同时用它把共享层（imaging/geometry/postprocess/render）与共享解析/容错模块（otsl/formula_repair/robustness）一次性建成。MinerU 是本项目的首要参考引擎，优先立起来。
>
> 说明：MinerU-vlm **不是单轮**协议，本阶段承担了"共享层从零建成 + 两阶段编排验证"双重任务，失败切分难度高于先做单轮——这是用户明确选择的"最难关先行"取舍（换取主力引擎最快落地、最重要风险最早暴露）。

| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-1.1 | 【Python 原型】mineru-vlm 两阶段闭环：版面请求→token 解析→逐区块裁剪→内容请求→OTSL/LaTeX，对真实 vLLM 跑通（附录 A） | P0 | L | 复刻 `mineru_report.md` 的两阶段协议，产出与 MinerU 官方结果可比对；记录 prompt/采样参数/原始输出样例作为 Rust 移植 golden 基准 |
| T-1.2 | `imaging.rs`：resize/pad/crop/RGB/base64/PNG 编码原子函数（MinerU 阶段①整页不保持长宽比拉伸、阶段②区块裁剪+短边下限放大+长宽比 padding 均是其参数组合） | T-0.6 | M | 各 resize 策略单测（含保持/不保持长宽比、像素上下限、padding） |
| T-1.3 | `geometry.rs`：按 `coordinate_system` 反归一化到像素帧、`Geometry::Rect`、IoU 去重、异常 block 隔离 + **`geom_frame` 嵌套坐标帧换算**（stage② 裁剪子图坐标→页面帧） | T-0.2 | L | 反归一化坐标与原图核对正确；行内公式等子块坐标正确换算回页面帧 |
| T-1.4 | `output_parse.rs` 的 `custom_token` 分支：MinerU `<\|box_start\|>...<\|ref_start\|>类别<\|ref_end\|>` 正则解析（含旋转、越界/畸形行跳过不中断） | T-0.2 | M | 真实版面输出解析，越界行跳过并计 warning |
| T-1.5 | `otsl.rs`：OTSL token 序列→HTML（rowspan/colspan） | T-0.2 | L | 真实 OTSL 样例转 HTML，与期望表格结构一致 |
| T-1.6 | `formula_repair.rs`：LaTeX 修复器链（括号平衡、重复 `\quad` 折叠、`\tag`/`\eqno`、`\begin{env}` 重建），可配置链 | T-0.2 | M | 对已知瑕疵样例逐条修复，回归快照 |
| T-1.7 | `robustness.rs`：重复 token/退化输出检测 + 递增 temperature 重试（供 dispatch 调用） | T-0.4 | M | 构造退化输出触发重试，达上限后降级不死循环 |
| T-1.8 | `category_map.rs`：MinerU 原生类别→统一枚举映射表 + 查表逻辑（含未知类别兜底） | T-0.2 | S | MinerU 全类别覆盖 |
| T-1.9 | `adapters/mineru_vlm.rs`：`parse_page` **两阶段编排**（版面 dispatch→`custom_token` 解析→逐区块裁剪+并发 dispatch→OTSL/formula→合并），`emitted_signals` 声明 spans/部分 merge_hint，`coordinate_system=Norm0To1000`，`provides_reading_order=true` | T-1.2~T-1.8,T-0.4 | L | 两阶段在单个 `parse_page` 内正确完成；区块级并发从**文档级并发预算**取令牌 |
| T-1.10 | `postprocess.rs` 分层实现（§4.1）：纯几何层（段落合并/表格 HTML 拼接）+ 信号增强层（按 `emitted_signals` 用 merge_hint/字号），跨页表格合并（HTML 行签名比对，`scraper`/`html5ever`） | T-1.9 | L | mineru-vlm（有信号）走增强层产出合理段落；跨页表格正确拼接；无信号时能优雅退化到纯几何层（P2 用 dots.ocr 验证退化路径） |

**Gate G1（最关键，前置）**：① **`parse_page` 单接口成功承载两阶段协议**（版面→解析→逐区块的依赖回路能在一个接口内完成，不需回退到 v0.8 的多方法接口）；② 真实 vLLM 上 `uparser parse doc.pdf --protocol mineru-vlm` 端到端出正确 Markdown；③ 共享层 + otsl/formula/robustness 建成可复用。**G1 不过说明 §2.0 的 `parse_page` 抽象有误，须回设计——这是全局最高风险点。**

---

### P2 — 第二协议（dots.ocr 单轮）：验证共享层"协议无关性"
> 目标：接入一个**结构最简单的单轮协议**作为第二个协议，其核心价值**不是**再解析一种文档，而是**反向证明 P1 为 MinerU 建的共享层不是 MinerU 专属**——如果 dots.ocr（单轮、JSON 输出、无 spans/merge_hint 信号）能在**不改动共享层任何一行**的前提下端到端跑通，就验证了 `postprocess.rs` 的纯几何层降级、`geometry.rs`、`category_map.rs`、`render/` 的协议无关性。

| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-2.1 | 【Python 原型】dots.ocr 单轮闭环：smart_resize→单次请求→JSON 解析→渲染，对真实 vLLM 跑通（附录 A） | P1 | M | 真实图片产出合理结果，记录 prompt/输出样例；产出截断/畸形样例喂 T-2.2 |
| T-2.2 | `output_parse.rs` 的 `strict_json` 分支 + 通用容错回退链（缺 `},{` 补齐、截断末项、去重、多级降级抢救单字段）——抽为通用容错原语，供未来 JSON 类协议复用 | T-1.4 | M | 用真实截断样例解析成功率达标 |
| T-2.3 | `adapters/dots_ocr.rs`：`parse_page` **单轮**实现（smart_resize→单次 dispatch→`strict_json` 解析），`coordinate_system=Norm0To1000`，`provides_reading_order=true`，`emitted_signals` 声明**无** spans/merge_hint | T-2.2,T-1.2,T-1.3 | M | 单图端到端产出 `Vec<Block>` |
| T-2.4 | `category_map.rs`：dots.ocr 原生类别→统一枚举映射表（含未知兜底） | T-1.8 | S | 全类别覆盖 |

**Gate G2（跨协议成立性，全局关键）**：**同一套 `geometry.rs`/`category_map.rs`/`postprocess.rs`/`render/` 在不改动前提下同时正确处理 mineru-vlm（两阶段、带信号）与 dots.ocr（单轮、无信号）两种 IR**；`postprocess.rs` 的"信号增强层 ↔ 纯几何层"降级由这两个协议实测覆盖。**G2 不过说明共享层隐含了 MinerU 特定假设，须把该假设显式化为 adapter capability（§4.1），而非硬编码在共享层。**

---

### P3 — MonkeyOCR-v2 + 共享模块复用验证
> 目标：第三个协议接入，验证"新增协议不改共享层"的可扩展性目标，并复用 P2 建好的 otsl/formula/robustness。

| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-3.1 | 【Python 原型】MonkeyOCR-v2 两阶段闭环 + `eval()` 式输出解析，对真实 vLLM 跑通（附录 A，注意 eval 安全，见 T-3.3） | P2 | M | 复刻 `monkeyocrv2_report.md` 协议 |
| T-3.2 | `output_parse.rs` 的 `python_literal_eval` 分支：**手写宽松字面量解析器**（单引号、`None`/`True`/`False`、括号配对容错、末尾未闭合补全、多候选择优；复用 T-2.2 的通用容错原语） | T-2.2 | L | 真实截断输出解析成功率达标；**Rust 侧绝不用任何 eval，纯解析器** |
| T-3.3 | 安全审查：Python 原型阶段禁用真实 `eval()` on 模型输出（改用 `ast.literal_eval` 或同款解析器），文档标注 | T-3.1 | S | 原型无 `eval()` 调用注入面 |
| T-3.4 | `adapters/monkeyocr.rs`：`parse_page` 两阶段（复用 P1 建的 otsl/formula/robustness），`--end2end` 可选单轮模式，`provides_reading_order=true`，等比缩放预处理 | T-3.2 | M | 端到端产出；复用 T-1.5/1.6/1.7（otsl/formula/robustness）无重复实现 |
| T-3.5 | 去弯曲预处理开放问题决策（§8）：默认走方案(a) 跳过（`emitted_signals`/`capability_notes` 标注"未去弯曲"），把(b)外部化列为后续 | T-3.4 | S | 决策写入文档；跳过路径不影响主干 |

**Gate G3**：接入第三个协议**未改动** geometry/category_map/postprocess/render 任何一行；otsl/formula/robustness 被 mineru-vlm 与 monkeyocr 两协议共用（代码复用可核查）。

---

### P4 — native 零模型协议（可与 P2 并行）
| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-4.1 | 内嵌 liteparse crate 依赖，把其 `ParsedPage` 转换为统一 `Block`/`Page`（`source=native_text_layer`，`coordinate_system=PixelAbs`，`provides_reading_order=true`） | P1 | M | 数字原生 PDF 走 native 出正确 Markdown |
| T-4.2 | 责任分界（§10.3）：把 liteparse 配置为"输入已是 PDF、勿自转"，格式转换归 uparser 的 normalize_format（P7）；局部 OCR 复用 liteparse `OcrEngine` 透传 | T-4.1 | M | native 不触发 liteparse 自带转换；局部 OCR 参数可透传 |
| T-4.3 | 把 `native` 做成 Cargo feature（默认开，`--no-default-features` 可去 liteparse/PDFium/Tesseract），记录各 feature 二进制体积 | T-4.1 | S | 关闭 feature 后二进制不含 liteparse；体积数据入发布说明 |

**Gate G4**：数字原生 PDF 零模型、零外部服务出正确 Markdown，延迟远低于 VLM 协议；feature 开关有效。

---

### P5 — pipeline 协议 + 逐 stage 后端
| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-5.1 | `ModelStage`/`StageBackend`（§2.1）类型与 `model_stages()` 语义（"可独立配置后端的部署单元"），CLI 逐 stage 参数（`--layout-backend` 等） | P2 | M | `uparser protocols` 列出 pipeline 四 stage 及 resource_hint |
| T-5.2 | **验证 layout/ocr/formula 的 ONNX 可行性**（§8 开放问题）：确认 PP-DocLayoutV2/PaddleOCR-torch/Unimernet 是否有可用 ONNX 导出；无则强制 Remote | T-5.1 | M | 结论文档化；`allows_local` 据实设定 |
| T-5.3 | `ort` 集成 + `table` stage 本地推理（SLANet/Unet/PP-LCNet，MinerU 原生 ONNX） | T-5.1 | L | table stage 本地跑通，输出表格 HTML |
| T-5.4 | **Pipeline Model Serving REST 契约设计**（§11.3，参照 liteparse `OCR_API_SPEC.md`）：layout/ocr/formula 的请求/响应 schema 规范文档 | T-5.1 | M | 契约文档评审通过 |
| T-5.5 | pipeline 的 Remote stage 客户端（`dispatch` 覆盖，走自定义 REST，复用 transport 并发/重试骨架） | T-5.4,T-0.4 | M | 对 mock serving 跑通 layout/ocr/formula |
| T-5.6 | `adapters/pipeline.rs`：`parse_page` 按 stage 顺序 layout→ocr→formula→table，各依 backend 走 Local/Remote，`provides_reading_order=false` | T-5.3,T-5.5 | L | 端到端出结果；Remote stage 不可达时明确报错不静默回退 Local |
| T-5.7 | 参考实现一个最小 Pipeline Model Serving（包装 MinerU pipeline 模型加载） | T-5.4 | L | 可作为 `--*-endpoint` 后端联调 |

**Gate G5**：table 走本地 `ort`、layout/ocr/formula 走 Remote 时，客户端 CPU/内存占用显著低于"全本地"，且结果正确（回答用户提出的"大模型吃客户端 CPU"问题）。

---

### P6 — PaddleOCR + 阅读顺序回退
| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-6.1 | PaddleOCR/PaddleX 服务契约确认（§8）：选定部署形态与请求/响应 schema | P2 | M | 契约确认文档 |
| T-6.2 | `reading_order.rs`：几何回退排序（xycut 风格或复用 liteparse 空间投影），供 `provides_reading_order=false` 协议 | P2 | M | 无序 block 集合排出合理阅读顺序 |
| T-6.3 | `geometry.rs` 的 `Geometry::Polygon` 处理：postprocess 取外接矩形参与几何、render 保留原多边形 | T-1.3 | S | polygon 检测框正确纳入段落合并 |
| T-6.4 | `adapters/paddleocr.rs`：检测框预处理/识别归一化、`ocr_boxes` 输出解析、无分类映射为 text、`dispatch` 覆盖走专属 REST | T-6.1,T-6.2,T-6.3 | L | 端到端出结果；`capability_notes` 标注"无版面分类" |

**Gate G6**：非原生阅读顺序协议经 `reading_order.rs` 得到合理顺序；polygon 几何被正确处理，未污染矩形几何逻辑。

---

### P7 — 多格式接入（可与 P2 并行）
| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-7.1 | `ingest.rs::detect_format()`：按内容/扩展名判定原始格式（不做转换） | P1 | S | 各格式正确识别 |
| T-7.2 | `ingest.rs::structured_bypass`（§12.2/§13.1a 第2步）：XLSX/CSV 用 `calamine` 直接读单元格→`Block`(`source=structured_native`, html 拼装)，短路后续 | T-7.1,T-0.2 | M | xlsx 直出结构化表格，不经任何模型 |
| T-7.3 | `ingest.rs::normalize_format()`：DOCX/PPTX/图片经 LibreOffice headless/ImageMagick 转 PDF；缺工具时退出码 2 + 明确提示 | T-7.1 | M | PPT/Word 转 PDF 后可走任一协议 |
| T-7.4 | 控制流串联（§13.1a）：`detect→structured_bypass?→normalize→rasterize` 唯一顺序落地 | T-7.2,T-7.3,T-0.6 | S | 顺序集成测试；xlsx 不进 normalize |
| T-7.5 | LibreOffice 转换性能/失败率评估（Agent 高频场景延迟预期，§8） | T-7.3 | S | 评估报告；超时/失败降级策略 |

**Gate G7**：PPT/Word 端到端可解析；xlsx 结构化旁路直出且不触发模型调用；控制流顺序唯一无歧义。

---

### P8 — Profiler + Router + auto（依赖 P4+P7）
| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-8.1 | `profiler.rs` L1（格式/元数据）+ L2（结构启发式：文本/图像密度、疑似表格区占比、宽高比、文本层有无；参考 liteparse `is_complex()`），产出 `DocumentProfile` | P7 | L | L2 纯计算无模型调用；书籍/PPT/长文本正确初判 |
| T-8.2 | `router.rs` v1 路由表（§13.4，文档级）：native vs VLM vs pipeline 分流 + 回退默认 + `RoutedBy` 记录 | T-8.1,P4 | M | 路由决策符合策略表；结果含 DocumentProfile |
| T-8.3 | `uparser classify` 命令（只跑 profiler 出 DocumentProfile JSON） | T-8.1 | S | 独立可调用，Agent 可先看再覆盖 |
| T-8.4 | `--protocol auto`（classify+route+parse 串联）与 `--protocol <name>` 显式跳过并存；`--profile-level` 控制层级 | T-8.2 | M | auto 默认可用；显式协议跳过 profiler |
| T-8.5 | L3 深度分类（**opt-in**，§13.2a）：采样页缩略图低成本分类调用（V4），复用已栅格化中间产物避免重复往返；子类型主要写入 metadata | T-8.4,P9缓存 | M | 仅 `--deep-classify` 触发；不与 parse 重复栅格化 |

**Gate G8**：L1/L2 免费启发式正确把"数字原生长文本→native、复杂/扫描→VLM"分流（覆盖最有价值分叉）；DocumentProfile 随结果透明输出。

---

### P9 — 缓存 + 流式 + doctor + 契约测试
> 注：`cache.rs`（T-9.1）建议提前到 P2 之后并行开工（v0.9 §7 M14 提示其收益）。

| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-9.1 | `cache.rs`（§15）：内容哈希 Key（sha256+protocol+参数指纹）、完整 ParseResult 缓存、**profiler 中间产物分层子键**、TTL、`uparser cache stat/clear`、`--no-cache` | P2 | L | 同文档二次调用命中；中间产物可被 L3/parse 复用 |
| T-9.2 | 流式输出（§2.2）：`--stream` 按窗口/页输出 NDJSON；缓存按窗口写入，中断可复用已完成窗口 | T-0.5,T-9.1 | M | 大文档边解析边出结果；中断续跑复用缓存 |
| T-9.3 | `uparser doctor`：各协议健康检查（OpenAI 探 `/v1/models`、pipeline/paddle 探自身接口）；`--protocol pipeline` 探本机 CPU/内存给 Local/Remote 建议（启发式非门禁） | P5 | M | 各协议诊断可用；建议标注为非强制 |
| T-9.4 | `uparser protocols`：列出 adapter capabilities（外部服务/坐标系/阅读顺序/各 stage resource_hint/requires_local_model） | P5 | S | introspection 输出完整 |
| T-9.5 | 契约测试集：mock 模型响应 + 固定样例文档 → 期望 IR/Markdown 快照；覆盖两阶段、截断输出、部分页失败、能力降级、跨页表格 | P2,P3,P5,P6 | L | CI 跑通；协议升级回归可防 |
| T-9.6 | 多协议一致性测试（同文档不同协议解析，检查 IR/Markdown 结构一致性容差） | T-9.5 | M | 一致性报告 |

**Gate G9**：缓存命中率与流式内存曲线达标；契约测试覆盖关键容错路径；doctor/protocols 可作为 Agent 前置自检手段。

---

### P10 — 语言绑定 + 发布
| ID | 任务 | 依赖 | 规模 | 验收 |
|---|---|---|---|---|
| T-10.1 | `uparser-napi`（napi-rs）：导出 parse/classify，IR 序列化对齐 | P9 | M | Node 调用产出与 CLI 一致 |
| T-10.2 | `uparser-python`（PyO3/maturin）：同上 | P9 | M | Python 调用产出与 CLI 一致 |
| T-10.3 | `packages/node`、`packages/python`：CLI 封装 + 原生二进制加载（复刻 liteparse packages 结构） | T-10.1,T-10.2 | M | `npm`/`pip` 装后 CLI 可用 |
| T-10.4 | 发布物：各 feature 组合二进制体积/依赖说明、Pipeline Model Serving 部署文档、示例 | T-10.3,T-7.5 | M | 文档齐全，可复现部署 |

**Gate G10**：Node/Python 绑定调用同一 core，三端（CLI/Node/Python）IR 序列化一致。

---

## 3. 关键验证门汇总（不过不进）

| Gate | 判定的架构假设 | 失败意味着 |
|---|---|---|
| **G1** | `parse_page` 单接口承载**两阶段协议**（mineru-vlm）；共享层 + otsl/formula/robustness 建成 | §2.0 的 `parse_page` 抽象有误，回设计（最高优先级风险，最难关先行） |
| **G2** | 共享层（geometry/category_map/postprocess/render）**协议无关**——mineru-vlm 与 dots.ocr 两种 IR 都能不改共享层跑通；信号增强↔纯几何降级成立 | 共享层隐含 MinerU 特定假设，须显式化为 adapter capability（§4.1），非硬编码 |
| G3 | 新增第三协议不改共享层 | 可扩展性目标未达成，adapter 边界划错 |
| G5 | stage 级 Local/Remote 卸载真能降客户端负载 | 卸载设计无收益，回 §11 |
| G8 | 免费 L1/L2 启发式能覆盖最有价值路由分叉 | Profiler 价值不足，需重估 L3 依赖度 |

---

## 4. 风险登记（映射 `ARCHITECTURE.md` §8 开放问题 → 处置任务）

| 风险 | 处置任务 | 阶段 |
|---|---|---|
| layout/ocr/formula 无 ONNX 导出，Local 不可行 | T-5.2 先验证，无则强制 Remote | P5 |
| Pipeline Model Serving 无行业标准契约 | T-5.4 自行设计并规范化 | P5 |
| PaddleOCR 服务契约不定 | T-6.1 先确认 | P6 |
| 共享 postprocess 隐含某协议假设 | G2 交叉验证 + `emitted_signals` 降级 | P2 |
| 标题分级多数模型不产层级 | 默认扁平 + 可选 `--title-leveling-endpoint`（§14），`capability_notes` 标注 | P2/P8 |
| 图表"数据反推"被误解为精确提取 | render 标 `description` 非 `extracted_data`，`capability_notes` 明示 | P8 |
| LibreOffice 转换性能/失败率 | T-7.5 评估 + 降级策略 | P7 |
| MonkeyOCR-v2 去弯曲模型无法外部化 | T-3.5 默认跳过（方案 a），外部化列后续 | P3 |
| Python 原型 `eval()` 注入面 | T-3.3 禁真实 eval，用解析器 | P3 |
| doctor 资源建议只是启发式 | T-9.3 标注非门禁 | P9 |
| 单文档混合内容页/区域级路由缺失 | 明确 v1 不做，记入后续版本 backlog | — |

---

## 5. 单元测试与测试策略

> 原则：core 的绝大部分是**纯函数式的数据处理**（解析、几何变换、格式转换、渲染），天然适合单元测试；唯一有副作用的部分（HTTP 派发、外部工具调用、文件 IO）用 **mock/替身**隔离，使**全部测试无需 GPU、无需真实 vLLM 即可在 CI 跑**。真实模型只在 Python 原型轨（附录 A）和少量手动冒烟里出现，不进 CI。
>
> **每个任务的"验收"隐含包含其单元测试**——下文 §5.4 是逐模块的最小测试清单，任务未附对应单测视为未完成（DoD 的一部分）。

### 5.1 测试分层（金字塔）

| 层级 | 范围 | 是否需网络/模型 | 工具 | 归属 |
|---|---|---|---|---|
| L0 单元测试 | 单模块纯函数（otsl/formula_repair/geometry/output_parse/category_map/imaging/reading_order…） | 否 | `cargo test` + `insta`(快照) + `proptest`(属性) | 随对应任务 |
| L1 模块集成 | adapter 的 `parse_page` 用 **mock transport**（喂固定原始响应），走到 `Vec<Block>` | 否（mock HTTP） | `wiremock`/自建 `MockDispatch` | 各 adapter 任务 |
| L2 契约/快照 | 固定样例文档 + 固定模型响应 → 期望 IR/Markdown 快照 | 否 | `insta` 快照 + 夹具库 | T-9.5 |
| L3 跨协议一致性 | 同文档不同协议解析，检查 IR/Markdown 结构一致性容差 | 否 | 自建断言 | T-9.6 |
| L4 端到端冒烟 | 真实 vLLM/服务，手动或 nightly，**不进 PR CI** | 是 | 脚本 | 手动/nightly |

### 5.2 测试工具选型（Rust）

| 用途 | crate | 说明 |
|---|---|---|
| 单元/集成测试框架 | 内置 `#[test]` + `#[tokio::test]` | async 测试用 tokio |
| 快照测试 | `insta` | OTSL→HTML、Markdown 渲染、IR JSON 等"输出结构固定"的断言，改动可 review diff |
| HTTP mock | `wiremock` | `transport.rs` 的并发/重试/超时、adapter 的 dispatch 替身 |
| 属性测试 | `proptest` | 几何反归一化、容错解析器（对任意畸形输入不 panic）、坐标帧换算的不变量 |
| 覆盖率 | `cargo-llvm-cov` | CI 出覆盖率报告与门槛 |
| 基准（可选） | `criterion` | 大文档解析吞吐、并发调度（非 CI 门，趋势监控） |

### 5.3 测试夹具（fixtures）

- **`tests/fixtures/raw/`**：各协议**真实原始模型输出样例**（来自附录 A 的 Python 原型），含**正常 / 截断（finish_reason=length）/ 畸形**三类，每类多条。这是容错解析测试的核心资产——`output_parse`/`otsl`/`formula_repair` 的边界 case 都来自真实退化输出，而非臆造。
- **`tests/fixtures/docs/`**：小型样例文档（数字原生 PDF、扫描件、含表格/公式/图表的 PDF、DOCX/PPTX/XLSX 各一），用于 ingest/profiler/端到端契约测试。
- **`tests/fixtures/golden/`**：期望的 IR（`ParseResult` JSON）与 Markdown 快照，由 `insta` 管理。
- **`MockDispatch` 替身**：实现与 `ParseCtx.dispatch` 相同签名，按请求返回 `fixtures/raw/` 里预置的响应——让 adapter 的两阶段编排能**离线**测试（stage① 返回预置版面、stage② 按区块返回预置内容），无需真实模型。这是 L1 层的关键基础设施，**在 P0（T-0.3/T-0.4）就要搭好**。

### 5.4 逐模块单元测试清单（最小集）

| 模块 / 任务 | 必备单元测试用例 |
|---|---|
| `imaging.rs`（T-1.2） | 等比/非等比 resize 尺寸正确；28 对齐；像素上下限裁剪；短边下限放大；长宽比超限 padding；RGB 转换；PNG/base64 往返 |
| `geometry.rs`（T-1.3） | 三种坐标系反归一化数值正确；`Rect` IoU 去重（含完全重叠/部分/不相交）；**`geom_frame` 嵌套换算**（裁剪帧→页面帧往返一致）；越界/负坐标钳制；异常 block 隔离不 panic |
| `output_parse.rs` custom_token（T-1.4） | 正常行解析；旋转 token；越界行跳过并 warning；空输出；行内混杂噪声；**proptest：任意字节串不 panic** |
| `otsl.rs`（T-1.5） | 简单表；rowspan/colspan；嵌套/不规则；空单元格；截断 OTSL 的容错；快照对比 HTML |
| `formula_repair.rs`（T-1.6） | 每个修复器单独的 before→after；修复器链幂等性（跑两次结果稳定）；正常公式不被破坏；`\begin{env}` 重建 |
| `robustness.rs`（T-1.7） | 检测到重复 token；正常输出不误判；重试温度递增序列正确；达上限停止不死循环 |
| `category_map.rs`（T-1.8/T-2.4/…） | 每个 adapter 全类别映射命中；未知类别兜底；空类别（PaddleOCR）映射为 text |
| `output_parse.rs` strict_json（T-2.2） | 合法 JSON；缺 `},{` 补齐；截断末项丢弃；重复去重；多级降级抢救单字段；**proptest 不 panic** |
| `output_parse.rs` python_literal_eval（T-3.2） | 单引号字典；`None/True/False`；括号未闭合补全；**断言绝不调用 eval**；proptest 不 panic |
| `postprocess.rs`（T-1.10） | 纯几何层段落合并（对齐/字号缺失时）；信号增强层（有 merge_hint/字号时更准）；**同输入下"有信号 vs 无信号"两条路径都产出合理结果**；跨页表格行签名比对合并；标题默认扁平 |
| `reading_order.rs`（T-6.2） | 单栏/多栏/混排的几何排序；polygon 取外接矩形参与；空输入 |
| `render/`（T-0.8） | Markdown/JSON/content-list 三格式快照；表格 HTML 内嵌；公式定界符；图片引用；`capability_notes` 透传 |
| `ingest.rs`（T-7.x） | `detect_format` 各格式识别；xlsx 结构化旁路直出 Block（不经模型）；normalize 顺序（xlsx 不进 normalize）；缺 LibreOffice 时退出码 2 |
| `scheduler.rs`（T-0.5） | 处理窗口分批；并发预算令牌不超上限；**单页失败隔离**（mock 一页抛错，其余页正常且计入 page_errors）；峰值内存 ~O(窗口)（可用计数断言近似） |
| `transport.rs`（T-0.4） | 并发限流命中信号量上限；4xx/5xx 重试+退避；超时；截断响应（finish_reason=length）容忍；连接失败明确错误 |
| `cache.rs`（T-9.1） | 相同 Key 命中；参数变化不命中；中间产物子键命中；TTL 过期；`--no-cache` 绕过 |
| `profiler.rs`（T-8.1） | L1 格式判定；L2 文本/图像密度、表格区占比、宽高比计算数值正确；数字原生长文本→native 分流；L3 默认关闭 |
| `router.rs`（T-8.2） | 路由表每条命中；xlsx 不进 router；无法判断回退默认协议 + warning |
| adapter 的 `parse_page`（各 T-x.x，L1 层） | 用 `MockDispatch`：dots.ocr 单轮走通；**mineru-vlm/monkeyocr 两阶段——mock stage① 版面→驱动 stage② 逐区块→合并正确**（离线验证两阶段编排，是 G1 的可自动化部分）；单区块失败隔离；截断响应容忍 |

### 5.5 契约/快照与跨协议一致性测试（对应 T-9.5 / T-9.6）

- **契约快照**：`fixtures/docs/` × 每个协议（用 `MockDispatch` 喂 `fixtures/raw/`）→ `insta` 快照 IR 与 Markdown。协议升级或共享层改动导致快照变化时，diff 必须被人工 review 后 `cargo insta accept`，防止悄悄回归。
- **必覆盖的容错场景**（来自 §3 Gate 与 §4 风险）：两阶段编排、截断输出、单页/单区块失败、能力降级（无分类/标题扁平/图表仅描述）、跨页表格合并边界、polygon 几何。
- **跨协议一致性**：同一 `fixtures/docs/` 文档分别用 mineru-vlm 与 dots.ocr 解析，断言页数、主要 block 数量级、Markdown 结构（标题/段落/表格数量）在容差内一致——这是 G2"共享层协议无关"的自动化回归化身。

### 5.6 CI 门与覆盖率

- **PR 门（必过）**：`cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`（L0-L3 全绿，无网络）、`insta` 无未接受快照。
- **覆盖率门**：核心纯函数模块（otsl/formula_repair/geometry/output_parse/category_map/reading_order）行覆盖 ≥ 85%；adapter 的 `parse_page` 分支覆盖两阶段/单轮/失败路径。`cargo-llvm-cov` 出报告，低于门槛 CI 失败。
- **nightly（非阻塞）**：L4 端到端冒烟（需 vLLM 的环境）、`criterion` 基准趋势。

### 5.7 与任务的关系（DoD）

- 每个 §2 的实现任务，其"验收"列的功能断言**必须由对应 §5.4 单测覆盖**才算完成；PR 描述里注明覆盖了哪些 case。
- `MockDispatch` 与 `fixtures/` 目录骨架在 **P0（T-0.9）** 就建立，使 P1 起的每个 adapter 都能立即写 L1 离线测试，而不是等到 P9 才补测试。
- **T-9.5（契约测试集）不是"最后补测试"，而是从 P1 起持续累积**——每接一个协议就往契约快照里加该协议的样例；P9 只是把它系统化 + 补跨协议一致性 + 完善 CI 门。

---

## 附录 A：Python 原型并行轨（§9.4）

每个生成式协议在写 Rust adapter **之前**，先用 Python 脚本对**真实部署的 vLLM 服务**跑通"预处理→请求→解析→渲染"最小闭环，用途：

1. 复刻并锚定该协议的 prompt 文本、采样参数、原始输出格式（作为 Rust 移植的 golden 基准与契约测试样例来源）。
2. 快速试错正则/解析逻辑（动态语言迭代快），稳定后再移植 Rust。
3. 产出真实的"截断/畸形输出"样例，喂给 `output_parse.rs` 的容错测试。

原型顺序对齐协议接入顺序（最难关先行）：**T-1.1（mineru-vlm 两阶段，首个）→ T-2.1（dots.ocr 单轮，验证共享层协议无关）→ T-3.1（monkeyocr-v2）**。原型代码不进产品，仅作参考实现与测试夹具来源；**Rust 侧一律不用 eval/动态执行**（T-3.2/T-3.3）。

---

## 附录 B：建议的人力与并行安排（供排期参考，非硬约束）

- **主干串行**（1 名核心 + 评审）：P0 → P1（mineru-vlm，**务必先过 G1——两阶段编排能否成立是全局最高风险，最难关先行**）→ P2（dots.ocr，过 G2 证明共享层协议无关）。
- **可并行支线**（G1 后即可启动，因共享层已在 P1 建成）：P4（native）、P7（多格式）与 P2 并行；P9 的 `cache.rs`（T-9.1）建议 P1 后即并行。
- **G2 通过后扇出**：P3/P5/P6 三协议线可并行推进（各自独立 adapter，共享层已冻结、已被两协议证明为通用）。
- **收口**：P8（依赖 P4+P7）→ P9 收尾 → P10 发布。

> 里程碑映射：本计划的 P0-P10 对齐 `ARCHITECTURE.md` §7 的 M0-M15，但按依赖与风险重排了顺序（如把缓存 T-9.1、Agent 契约 T-0.7 提前，把 L3 降为 opt-in 后置），并细化到可分配的 Task 粒度。
