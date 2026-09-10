# UParser Adapter 技术架构文档

## 1. 文档目的

本文描述 `uparser-core` 当前适配器体系的技术结构、运行时生命周期、职责边界、
扩展方式和测试要求。它回答以下问题：

- 什么是 UParser adapter；
- adapter 与协议、调度器、transport、后处理和 renderer 如何协作；
- 哪些逻辑必须由 adapter 独立实现，哪些逻辑必须进入共享层；
- 当前各内置 adapter 的差异；
- 新增一个模型协议时应该修改哪些位置；
- 如何避免并发、坐标、错误恢复和输出一致性问题。

本文以当前代码为准。主要实现位于：

- `uparser/crates/uparser-core/src/adapters/`
- `uparser/crates/uparser-core/src/protocol_spec.rs`
- `uparser/crates/uparser-core/src/runner.rs`
- `uparser/crates/uparser-core/src/scheduler.rs`
- `uparser/crates/uparser-core/src/transport.rs`
- `uparser/crates/uparser-core/src/types.rs`
- `uparser/crates/uparser-core/src/postprocess.rs`
- `uparser/crates/uparser-core/src/ascend.rs`
- `uparser/crates/uparser-core/src/render/`

## 2. 核心定义

Adapter 是“外部解析协议到 UParser 统一 IR 的边界层”。

它不是完整文档渲染器，也不应该成为另一套独立流水线。它的核心职责是：

1. 根据协议构造请求；
2. 编排协议所需的一个或多个推理阶段；
3. 解析模型或服务的原始响应；
4. 将协议坐标转换到页面像素坐标；
5. 将协议原始类别映射到统一类别；
6. 将文本、表格、公式、图片和错误写入统一 `Block`；
7. 保留协议确实提供的阅读顺序和语义信号；
8. 对局部失败进行隔离并输出可诊断信息。

Adapter 的输出不是 Markdown，而是：

```text
Vec<Block>
```

统一 IR 之后的段落合并、语义提升和渲染由共享层负责。

## 3. 总体架构

```text
CLI / Rust API / Node / Python
              |
              v
      PreflightSource
  格式识别、摘要、输入身份
              |
              v
      Profiler + Router
   显式协议或 auto 路由
              |
              v
        PreprocessPlan
 原生语义 / 页面栅格 / 转换
              |
              v
     Registry.build(name)
   endpoint/model 配置覆盖
              |
              v
          Scheduler
  页面窗口、任务隔离、进度
              |
              v
   ProtocolAdapter.parse_page
 协议请求、解码、归一化到 IR
              |
              v
           Page/Block IR
              |
              v
       shared postprocess
 文本规范化、段落及信号合并
              |
              v
             assets
    图片落盘、路径写回、去重
              |
              v
             ascend
 Block IR -> CanonicalDocument
              |
              v
             render
 Markdown / JSON / document-json
```

`native` 是主要例外：它由 runner 进入整篇文档执行路径，而不是依赖普通的
逐页模型请求。为了注册表和能力清单一致，它仍实现 `ProtocolAdapter`，但
`parse_page()` 明确返回“不支持”，真实入口是 native document execution。

## 4. ProtocolAdapter 接口

当前 trait 定义位于 `adapters/mod.rs`：

```rust
#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn spec(&self) -> &'static ProtocolSpec;
    fn coordinate_system(&self) -> CoordinateSystem;
    fn provides_reading_order(&self) -> bool;
    fn category_vocab(&self) -> &[&'static str];
    fn raw_output_format(&self) -> RawOutputFormat;
    fn emitted_signals(&self) -> PostprocessSignals;
    fn model_stages(&self) -> Vec<ModelStage>;

    async fn parse_page(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError>;
}
```

### 4.1 元数据方法

| 方法 | 含义 |
| --- | --- |
| `name()` | registry、CLI 和结果中的稳定协议名称 |
| `spec()` | 从全局 catalog 返回声明式协议描述 |
| `coordinate_system()` | adapter 接收到的原始坐标体系 |
| `provides_reading_order()` | 协议是否提供可信顺序 |
| `category_vocab()` | 上游协议可能返回的原始类别集合 |
| `raw_output_format()` | 原始模型输出的编码形态 |
| `emitted_signals()` | adapter 能否输出 span、merge hint、font size |
| `model_stages()` | 可部署模型阶段和默认 backend 元数据 |

### 4.2 当前元数据的真实作用

这些方法当前主要用于：

- `uparser protocols` 的机器可读能力展示；
- registry 与 `ProtocolSpec` 的一致性测试；
- 测试中的协议能力断言；
- 未来执行器演进所需的声明式基础。

它们目前不会自动替 adapter 完成解码或后处理。例如声明
`RawOutputFormat::StrictJson` 不会让 runner 自动调用 JSON parser；adapter 仍需在
`parse_page()` 中显式调用对应解析函数。`emitted_signals()` 目前也没有直接传入
`postprocess_pages()`，共享后处理是根据每个 `Block` 的实际字段工作。

因此，`ProtocolSpec` 和 trait capability 是“可校验的声明”，不是完整的策略执行引擎。

### 4.3 parse_page 的输入输出契约

输入 `RenderedPage` 包含：

```rust
pub struct RenderedPage {
    pub page_num: u32,
    pub width: u32,
    pub height: u32,
    pub png_bytes: Vec<u8>,
}
```

输出必须满足：

- `Block.geom` 表达标准化后的几何形状；
- `Block.bbox_px` 使用当前页面像素坐标；
- `Block.geom_frame` 正确声明是页面还是 crop 坐标；
- `category_raw` 原样保留协议标签；
- `category` 使用 UParser 统一类别；
- 文本、HTML 和 LaTeX 分别写入 `text`、`html`、`latex`；
- 不能把 HTML 表格塞入普通 `text`；
- 不能把 display formula 当普通段落；
- `reading_order` 只在协议确实提供顺序时填写；
- 局部识别失败写入 `Block.error`；
- 图片内容通过 `asset_bytes` 暂存，不能嵌入 JSON；
- warnings 通过 `ParseCtx::warn()` 上报。

## 5. ProtocolSpec 声明模型

`protocol_spec.rs` 维护全部协议的静态 catalog。每个协议包含：

```rust
pub struct ProtocolSpec {
    pub name: &'static str,
    pub mode: ModeKind,
    pub shape: ProtocolShape,
    pub transport: TransportContract,
    pub preprocess: PreprocessKind,
    pub decode: DecodeKind,
    pub coordinates: CoordinateKind,
    pub order: OrderSource,
    pub default_endpoint: Option<&'static str>,
    pub requires_pdf_native_feature: bool,
}
```

### 5.1 执行形态

| ProtocolShape | 说明 | 典型协议 |
| --- | --- | --- |
| `NativeDocument` | 整篇文档在进程内解析 | native |
| `OneShotPage` | 每页一次完整识别 | dots-ocr、generic-vlm、tesseract |
| `LayoutThenRecognize` | 先布局，再按块识别 | mineru-vlm、monkeyocr-v2 |
| `StructuredService` | 服务端已完成结构融合 | paddleocr、paddlex-structure |
| `StageGraph` | core 编排多个独立模型阶段 | pipeline |
| `Mock` | 测试协议 | mock |

### 5.2 当前执行约束

runner 会读取 `ProtocolSpec` 并验证执行形态，但当前只有 `StageGraph` 会触发额外的
stage graph 校验。其他 shape 的实际调用轮次仍由 adapter 自己控制。

新增协议时，catalog 与 registry 必须同时登记。测试会检查 registry 中的每个协议都有
对应 `ProtocolSpec`，并检查坐标和阅读顺序声明一致。

## 6. Registry 与配置覆盖

`Registry` 保存协议名到 factory 的映射：

```text
protocol name -> AdapterFactory -> Arc<dyn ProtocolAdapter>
```

factory 而不是 adapter 单例的原因是每次执行都可能应用不同的配置覆盖：

- endpoint；
- model；
- pipeline 的阶段级 backend 和 endpoint。

公共覆盖对象是 `AdapterOverrides`。runner 在路由完成后才构造 adapter，这保证
`auto` 选择出的实际协议能够读取对应协议配置。

配置解析优先级为：

```text
CLI flag -> environment -> config[effective-protocol] -> adapter default
```

新增普通 HTTP 模型 adapter 时，应优先复用现有 `endpoint` 和 `model` 字段。只有当
协议确实需要公开额外参数，且参数影响行为或 cache 时，才扩展公共配置对象。

## 7. ParseCtx、Transport 与请求调度

`ParseCtx` 是 adapter 的运行时上下文，持有：

- real transport 或测试 MockDispatch；
- 文档级共享 semaphore；
- 文档级 warnings collector；
- cancellation token。

它提供三类 dispatch：

| 方法 | 用途 |
| --- | --- |
| `dispatch()` | OpenAI Chat Completions |
| `dispatch_rest()` | 自定义 JSON REST 服务 |
| `dispatch_binary()` | tensor wire 二进制服务 |

`Transport` 统一承担：

- HTTP client；
- timeout；
- retries；
- exponential backoff 与 jitter；
- `Retry-After`；
- 429/5xx 恢复；
- 总 wall-clock backstop；
- JSON 响应解析。

Adapter 不应自己创建 `reqwest::Client` 或重新实现重试逻辑。协议只负责构造请求和解释响应。

### 7.1 MockDispatch endpoint fragment

多阶段 adapter 通常给测试 endpoint 增加 fragment，例如：

```text
http://host/v1/chat/completions#layout
http://host/v1/chat/completions#recognize:3
```

fragment 用作 MockDispatch 的稳定 key。真实 transport 发请求前会使用实际 endpoint
语义，测试则可精确区分每个阶段和 block。新增 adapter 应保持 key 可预测，便于离线验证
完整编排。

## 8. 并发模型

并发分为两个不同维度：

1. `Scheduler.window_size` 控制一次生产并持有多少 rasterized pages；
2. `max_concurrency` 对应共享 semaphore，限制同时进行的网络请求数。

Scheduler 会并发调用多页的 `parse_page()`，但不会为整个页面预先占用网络 permit。
这是两阶段协议必须遵守的关键约束。

正确模式：

```text
decode/crop/resize/encode
        |
        v
ctx.acquire_permit()
        |
        v
ctx.dispatch(request)
```

错误模式：

```text
页面开始时占用 permit
        |
        v
布局完成后等待 block permit
        |
        v
所有页面互相等待，产生嵌套 semaphore 死锁
```

CPU 图像处理不应长期占用网络 permit。两阶段 adapter 的 block futures 应通过共享收集器
并发执行，最终按原 block index 归并，不能按响应完成顺序改变阅读顺序。

## 9. 错误、告警与取消

### 9.1 错误层级

| 层级 | 表达方式 | 行为 |
| --- | --- | --- |
| 文档准备失败 | `PrepareError` | 不进入 adapter |
| 页面生产失败 | `PageSourceError` | 终止或取消后续窗口 |
| 页面协议失败 | `PageError` | 当前页进入 `page_errors`，其他页继续 |
| 单块识别失败 | `Block.error` | 保留当前页与其他块 |
| 可恢复异常 | `ParseCtx::warn()` | 继续执行并进入 `ParseResult.warnings` |
| adapter task panic | scheduler 转 `PageError` | 不传播并终止整篇文档 |

以下情况通常属于页面错误：

- 页面图片无法解码；
- layout 请求完全失败；
- 响应没有可提取的主 content；
- 页面级服务契约完全不成立。

以下情况通常属于 block 错误或 warning：

- 某个 recognition 请求失败；
- 某一布局行畸形；
- 某个 bbox 越界但可 clamp；
- 未知类别降级为 `unknown`；
- OTSL 局部不完整；
- `finish_reason=length` 但仍可恢复部分输出。

### 9.2 取消

Adapter 应通过 `ParseCtx` dispatch，而不是绕过它，以便 cancellation token 可以中止等待中的
HTTP 请求。进行较长的 adapter 内部循环时也应检查取消状态，尤其是在生成大量 block 请求前。

## 10. 统一 Block IR

Adapter 的首要产物是 `Block`：

```rust
pub struct Block {
    pub geom: Geometry,
    pub geom_frame: CoordFrame,
    pub bbox_px: Option<[i32; 4]>,
    pub category_raw: String,
    pub category: Option<String>,
    pub reading_order: Option<u32>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub latex: Option<String>,
    pub spans: Vec<Span>,
    pub merge_hint: Option<MergeHint>,
    pub confidence: Option<f32>,
    pub source: BlockSource,
    pub error: Option<String>,
    pub asset_bytes: Option<Vec<u8>>,
    pub asset_path: Option<String>,
    pub asset_caption: Option<AssetCaption>,
}
```

### 10.1 几何契约

`geom` 可以是矩形或多边形：

```rust
Geometry::Rect([x0, y0, x1, y1])
Geometry::Polygon(Vec<[x, y]>)
```

`bbox_px` 是便于通用算法消费的页面像素外接矩形。即使 `geom` 是 polygon，也应在可计算时
提供 `bbox_px`。

`geom_frame` 用于区分页坐标和 crop 坐标。adapter 最终输出的主要 layout blocks 通常必须
转换为 `CoordFrame::Page`。如果保留识别阶段内部 span，则必须正确记录其 crop parent 和
crop bbox，不能把 crop 内坐标误报成页面坐标。

### 10.2 内容字段契约

| 内容 | 字段 |
| --- | --- |
| 普通文本、标题、代码、caption | `text` |
| 结构化表格 | `html` |
| 公式 | `latex` |
| 图片二进制 | `asset_bytes`，落盘后变为 `asset_path` |
| 失败说明 | `error` |

同一个 block 可以有多个字段，但 adapter 应避免内容重复。例如已经转换为 HTML 的表格不应再
把同一份 OTSL 放进 `text`，否则 renderer 或下游消费者可能输出两次。

### 10.3 原始类别与统一类别

`category_raw` 是可诊断的协议事实，必须保留。`category` 是共享后处理和 renderer 的稳定语义。

类别映射应放在 `category_map.rs` 的协议专属函数中：

```text
map_mineru_vlm_category
map_dots_ocr_category
map_monkeyocrv2_category
map_pipeline_category
```

未知标签不应 panic，也不应静默伪装成 text。推荐降级到 `unknown` 并记录 warning。

## 11. 三层处理边界

Adapter 体系不是“每个 adapter 一套完整后处理”，而是三层分工。

### 11.1 第一层：协议专属解码与归一化

必须留在 adapter 或协议 parser 中：

- prompt 和 sampling；
- 单阶段或多阶段请求编排；
- 原始 JSON、token、Python literal 或服务 envelope 解码；
- 协议坐标和旋转语义；
- crop 策略；
- 类别词表；
- 上游明确规定的 container、skip、dedupe 规则；
- 模型输出字段到 `text/html/latex` 的分流；
- 协议特有的截断恢复。

判断标准：如果换一个模型协议，该规则会失效或改变，它就属于协议层。

### 11.2 第二层：共享 Block 后处理

当前入口是 `postprocess::merge_paragraphs_by_geometry()`，由 runner 对 adapter 输出统一调用。

当前职责包括：

- 文本内容规范化；
- 相邻 text block 的几何段落合并；
- 消费 `MergeHint::SameParagraph`；
- 合并 bbox 与 spans。

`--no-postprocess` 可以跳过这一层，主要用于诊断 adapter 原始 IR。

不应放入此层：某个模型独有的 token 解析、某协议独有的 bbox 缩放、某模型专用 prompt。

### 11.3 第三层：共享语义提升和渲染

`ascend.rs` 将几何 Block IR 恢复成 `CanonicalDocument`：

- title -> heading；
- 连续 list items -> list；
- table HTML -> canonical table；
- equation -> formula；
- image -> asset block；
- header/footer/page number 等页面附属内容按渲染策略过滤；
- caption 与 asset 关系进入统一文档模型。

随后 `uparser-document-engine` 的单一 renderer 生成 Markdown 或 document-json。

除 native PDF 的显式 legacy Markdown 路径外，所有模型协议和 pipeline 最终使用同一 renderer。

## 12. 共享基础组件

新增 adapter 前应优先检查以下模块：

| 模块 | 可复用能力 |
| --- | --- |
| `imaging.rs` | RGB、resize、crop、rotate、PNG/data URL |
| `geometry.rs` | 坐标回映、clamp、IoU、去重、polygon bounds |
| `output_parse.rs` | 按协议组织的原始输出 parser |
| `category_map.rs` | 原始类别到统一类别 |
| `otsl.rs` | OTSL 表格转换 |
| `formula_repair.rs` | 公式修复和 delimiter 规范化 |
| `content_normalize.rs` | 用户可见文本规范化 |
| `robustness.rs` | 重复生成检测和升温重试 |
| `shape_executor.rs` | 通用阶段 dispatch 和响应处理 |
| `reading_order.rs` | 几何阅读顺序工具 |
| `assets.rs` | 图片落盘、hash 去重、路径回写 |

共享的含义不是所有协议都必须调用，而是相同语义只能有一个权威实现。协议可以选择是否调用，
但不应复制一份稍有差异的实现，除非上游契约确实不同且有 fixture 证明。

## 13. 内置 Adapter 技术矩阵

| Adapter | 形态 | Transport | 原始输出 | 坐标/顺序 | 主要专属逻辑 |
| --- | --- | --- | --- | --- | --- |
| `native` | NativeDocument | InProcess | native artifact | source semantic | PDF/结构化文档整篇解析 |
| `tesseract` | OneShotPage | InProcess | OCR words/lines | pixel/model order | 本地 OCR、词行聚合 |
| `mineru-vlm` | LayoutThenRecognize | OpenAI chat | custom tokens | 0..1000/几何回退 | 两阶段、旋转、包含块过滤、merge hint |
| `dots-ocr` | OneShotPage | OpenAI chat | JSON cells | resized pixels/model order | smart resize、JSON 多级恢复 |
| `generic-vlm` | OneShotPage | OpenAI chat | whole-page Markdown | no geometry/model order | Markdown 回解析为 Block |
| `monkeyocr-v2` | LayoutThenRecognize | OpenAI chat | Python literal | 0..1000/model order | 两阶段、pixel bounds resize |
| `paddleocr` | StructuredService | JSON REST | OCR boxes | pixel/几何回退 | Paddle OCR envelope 解码 |
| `paddlex-structure` | StructuredService | JSON REST | Markdown envelope | no geometry/model order | 服务端融合结果回解析 |
| `pipeline` | StageGraph | JSON/binary stages | stage outputs | pixel/adapter computed | layout/OCR/formula/table DAG 与融合 |
| `mock` | Mock | None | synthetic | pixel/model order | 离线测试 |

### 13.1 MinerU、MonkeyOCR 与 dots.ocr

这三个 adapter 有各自的原始格式和图像预处理，但共同使用：

- `category_map` 中的协议函数；
- `geometry` 中的坐标工具；
- `otsl` 表格转换；
- `formula_repair`；
- 最终全局 postprocess、ascend 和 renderer。

MinerU 和 MonkeyOCR 的两阶段结构相似，也不意味着应合并为同一个 adapter。只有真实相同的
primitive 应共享；输出语法、prompt、skip 类型和 crop 规则仍属于协议。

### 13.2 generic-vlm 与 paddlex-structure

两者都可能接收完整 Markdown，再解析回统一 Block。由于原始结果缺少可靠 block geometry，
下游不能依赖 `bbox_px` 做布局算法。它们仍通过统一 renderer 输出，避免 API 和 CLI 使用不同
Markdown 语义。

### 13.3 pipeline

Pipeline adapter 的内部处理比普通 adapter 更深：core 自己编排 layout、OCR、formula 和 table
stage，并负责融合。这些属于协议执行而不是通用后处理。它完成 Block IR 后，仍进入相同的全局
postprocess 和 renderer。

## 14. 新增 Adapter 的标准步骤

### 14.1 先建立权威协议证据

在写代码前确认：

- 服务 endpoint 和鉴权；
- 请求 schema；
- system/user prompt；
- 图片编码、尺寸和排列顺序；
- sampling 参数；
- 输出 schema 或 grammar；
- 坐标原点、范围、轴顺序和相对 frame；
- 阅读顺序；
- 类别词表；
- 表格、公式、图片和容器语义；
- 截断、空输出和错误 envelope。

证据优先级：真实服务响应 > 上游推理代码 > 官方文档 > README 示例 > 推测。

### 14.2 定义协议身份

在 `protocol_spec.rs` 添加 `ProtocolSpec`，选择准确的：

- mode；
- shape；
- transport；
- preprocess；
- decode；
- coordinates；
- order；
- default endpoint。

如果现有 enum 无法准确表达协议，不应选择“最接近”的错误值，应增加新枚举并补齐测试。

### 14.3 实现专属 adapter

新增 `adapters/<protocol>.rs`：

1. adapter 配置结构和默认值；
2. request builder；
3. capability methods；
4. `model_stages()`；
5. `parse_page()`；
6. 邻近单元测试。

较复杂的输出 parser 放入 `output_parse.rs` 或独立 parser 模块，不要把所有 regex 和恢复链堆在
`parse_page()` 中。

### 14.4 注册公共入口

至少修改：

- `adapters/mod.rs` 的 module 和 factory；
- `runner.rs::canonical_protocol()`；
- CLI 帮助及协议列表测试；
- 默认 endpoint 一致性测试；
- 用户协议文档和配置示例。

Node/Python 当前使用字符串协议，通常无需扩展 ABI 枚举，但必须增加跨入口集成测试。

### 14.5 接入统一 IR

逐字段验证：

- geometry 是否在页面坐标；
- category 是否归一化；
- reading order 是否真实；
- table/formula 是否进入正确字段；
- image 是否保留 asset；
- errors/warnings 是否可见；
- source provenance 是否正确。

### 14.6 验证共享输出

同一个 fixture 至少检查：

- adapter 原始 Block IR；
- 开启 postprocess 后的 Page；
- Markdown；
- JSON；
- document-json；
- assets 路径和文件内容。

## 15. 测试分层

### 15.1 Parser 单元测试

- 正常响应；
- 空响应；
- 畸形、截断和重复响应；
- 未知字段和类别；
- 坐标边界；
- 版本漂移 fixture。

### 15.2 Adapter 离线测试

使用 `MockDispatch` 测试完整协议编排：

- 请求次数；
- stage endpoint key；
- prompt 和 sampling；
- skip 类型；
- 单块错误；
- warning；
- 异步结果顺序。

### 15.3 跨协议 contract test

不同协议输入语法不同，但等价内容进入统一 IR 后应满足相同核心语义。例如同一个标题、正文、
表格和公式，应该在 Markdown 中得到一致结构。

contract test 不要求丢弃协议独有信息。应比较统一字段，同时确认 `category_raw` 和 source 等
provenance 仍然不同。

### 15.4 Scheduler 与故障测试

- 多页窗口；
- block fan-out；
- 最大并发限制；
- 无嵌套 permit 死锁；
- 页面 panic 隔离；
- cancellation；
- progress callback；
- warnings 跨页汇总。

### 15.5 真实服务测试

Mock 只能验证编排，不能证明协议兼容。每个远程 adapter 都应至少有：

- 单页 smoke；
- 多页并发 smoke；
- table/formula/image 样本；
- 服务端错误和限流；
- 与上游原生客户端的输出对照。

## 16. Cache 与可复现性

执行 cache key 包含：

- source bytes；
- protocol；
- endpoint；
- model；
- execution fingerprint。

execution fingerprint 还覆盖窗口、并发、pipeline 配置、页面范围、后处理开关、资产选项和文档
解析 limits。

新增会改变解析结果的 adapter 参数时，必须把参数纳入 fingerprint。否则同一文档可能从 cache
取回由另一种 prompt、布局模式或预处理设置生成的结果。

协议实现或默认 prompt 发生不兼容变化时，也需要考虑 cache schema/version 策略，不能只依赖
协议名称不变。

## 17. Assets 生命周期

模型 adapter 识别到图片区域时：

1. adapter 对原始页面进行 crop；
2. PNG 暂存到 `Block.asset_bytes`；
3. `asset_bytes` 通过 serde skip 避免进入 JSON；
4. runner 调用 `assets::write_page_assets()`；
5. 文件按 SHA-256 命名，实现相同图片去重；
6. `Block.asset_path` 写入相对路径；
7. `asset_bytes` 清空；
8. ascend 把路径提升到 canonical asset；
9. renderer 生成图片链接。

Adapter 不应自行选择最终文件目录或直接写磁盘，否则会绕过 `--assets-dir`、`--no-assets`、
streaming 和 API 调用的统一行为。

## 18. 设计准则

### 18.1 应该留在 Adapter 的逻辑

- 协议请求；
- 协议解析；
- 模型坐标换算；
- 模型原始类别；
- 协议明确要求的多阶段关系；
- 影响识别输入的 crop、rotate、mask；
- 协议特有容错。

### 18.2 应该提升到共享层的逻辑

- 多个协议实际使用相同语义的 OTSL；
- 通用公式 delimiter 和括号修复；
- 通用坐标 clamp、IoU 和 polygon bounds；
- 通用图片编码；
- 文本规范化；
- 重试、timeout、backoff 和 cancellation；
- assets 落盘；
- IR 到 canonical document；
- renderer。

共享抽象的前提是至少两个真实协议存在相同行为，或者它是明确的协议无关基础设施。不要仅为
未来可能复用而提前设计复杂框架。

### 18.3 禁止的实现方式

- adapter 直接输出最终 Markdown 并绕过统一 IR；
- 每个 adapter 自建 HTTP client 和重试；
- 用字符串替换代替结构化 JSON/parser；
- 丢弃 `category_raw`；
- 未经验证声称模型提供 reading order；
- 把 crop 内坐标当页面坐标；
- 遇到一个坏 block 就丢弃整页；
- 将网络失败转换为空文本；
- 页面级占用 semaphore 后再等待 block permit；
- adapter 直接写 assets 到固定目录；
- 复制一套独立 Markdown renderer；
- 为单个协议污染全局 postprocess 的默认行为。

## 19. 当前技术债与演进方向

### 19.1 声明与执行尚未完全统一

`ProtocolSpec.decode`、`raw_output_format()` 和 `emitted_signals()` 当前主要用于描述和测试，尚未
形成由统一 executor 自动选择 decoder/postprocessor 的运行时机制。

短期应保持声明和实现严格一致；中期可以考虑让 shape executor 消费部分声明，但不能为了形式
统一而抹平真实协议差异。

### 19.2 trait 面向逐页协议，native 是例外

`ProtocolAdapter::parse_page()` 无法表达整篇原生文档解析。native 目前通过 runner 特判进入
`execute_native()`。如果未来出现更多 document-level adapter，应考虑引入明确的 document
adapter trait，而不是继续增加协议名特判。

### 19.3 协议专属配置扩展能力有限

`AdapterOverrides` 对普通 adapter 主要只有 endpoint/model。新增布局模式、语言、token budget
等配置时容易推动全局结构膨胀。后续可评估受 schema 约束的 adapter-specific config，但必须
保持 CLI、API、binding 和 cache fingerprint 一致。

### 19.4 Block IR 的层级表达有限

当前 IR 擅长扁平页面 block、span、list hint 和 asset caption，但对任意 container/child 关系
表达有限。面对 list container、table caption group、多层 figure 等协议时，adapter 需要谨慎
扁平化。

只有两个以上协议都证明需要同类层级关系时，才应扩展统一 IR；在此之前保留原始类别和顺序，
只移植影响最终语义正确性的最小关系规则。

### 19.5 能力声明应增加可执行验证

未来可以增加 invariants：

- `provides_reading_order=true` 时成功 block 应有稳定 order；
- `coordinates=Norm0To1000` 时 adapter 输出前必须完成 page-pixel 映射；
- `category_vocab` fixture 覆盖率；
- `emitted_signals` 与实际输出字段一致；
- `default_endpoint` 与 adapter default 一致。

## 20. Adapter 代码审查清单

### 协议正确性

- [ ] 请求和响应由上游代码或真实服务确认；
- [ ] prompt、sampling、图片顺序完全匹配；
- [ ] 输出版本漂移有 fixture；
- [ ] 原始类别完整；
- [ ] 坐标范围、轴、原点和 frame 明确；
- [ ] reading order 声明有证据。

### IR 正确性

- [ ] `geom` 和 `bbox_px` 一致；
- [ ] `category_raw` 未丢失；
- [ ] `category` 使用统一词表；
- [ ] text/html/latex 分流正确；
- [ ] source provenance 正确；
- [ ] 图片进入 asset 生命周期；
- [ ] container 不产生重复或空输出。

### 运行时安全

- [ ] 使用 ParseCtx transport；
- [ ] timeout 和 retry 有界；
- [ ] cancellation 生效；
- [ ] CPU 处理不占网络 permit；
- [ ] block fan-out 不会死锁；
- [ ] 页面错误隔离；
- [ ] 单块错误不吞掉整页；
- [ ] warning 对 CLI 和 API 都可见。

### 输出一致性

- [ ] 开启和关闭 postprocess 均可诊断；
- [ ] Markdown 走统一 renderer；
- [ ] JSON 不包含 asset bytes；
- [ ] document-json 可生成；
- [ ] 跨协议 contract test 通过；
- [ ] cache 参数完整。

### 工程完整性

- [ ] registry 和 ProtocolSpec 同步；
- [ ] canonical protocol 同步；
- [ ] doctor 和 protocols 可用；
- [ ] CLI、Rust API、Node/Python 路径验证；
- [ ] 文档和配置示例更新；
- [ ] `cargo fmt --all --check`；
- [ ] `cargo test -p uparser-core`；
- [ ] `cargo clippy -p uparser-core --all-targets -- -D warnings`；
- [ ] `cargo test --workspace`。

## 21. 结论

UParser adapter 的正确定位是协议边界和 IR 归一化器。每个 adapter 必须忠实处理自己的请求、
原始输出、坐标和模型语义，但不应拥有独立的最终后处理和渲染体系。

当前推荐结构是：

```text
协议专属请求与解码
        +
协议专属最小语义修复
        |
        v
统一 Block IR
        |
        v
共享 postprocess + ascend + renderer
```

这个边界既保留模型协议差异，也保证 CLI、API、不同模型和不同输出格式最终遵守同一个文档
语义契约。
