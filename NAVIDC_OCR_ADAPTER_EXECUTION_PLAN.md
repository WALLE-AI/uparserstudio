# NaviDC-OCR Adapter 执行方案

## 1. 目标与范围

在 `uparser-core` 中新增独立协议 `navidc-ocr`，对接
`opensource/NaviDC-OCR` 的 NaviDC-OCR 模型，使 CLI、Rust API、Node/Python
绑定可以通过现有统一执行链解析 PDF、PNG 和 JPEG，并输出统一 IR、Markdown、JSON
与 document-json。

本次适配遵循以下边界：

- 模型推理仍运行在外部服务中，`uparser` Rust 进程不加载 Python、PyTorch 或 vLLM。
- 使用项目现有 OpenAI Chat Completions transport 对接 NaviDC-OCR 服务。
- 新协议只复用通用图像、传输、调度、OTSL、公式修复和渲染能力，不复用其他模型的私有输出解析器。
- 第一版支持显式选择 `--protocol navidc-ocr`，不立即改变 `auto` 路由结果。
- `opensource/NaviDC-OCR` 只作为协议研究依据，不作为产品代码依赖或分发内容。

## 2. 调研结论

NaviDC-OCR 不是整页 Markdown 协议，而是专用的两阶段协议：

1. 布局阶段把页面固定缩放为 `1036x1036`，使用 `Analyze the image layout.`
   或 `Multi-point Layout Segmentation Analysis.` prompt。
2. 模型逐行返回 `<box:...><label:...><方向>`。
3. 坐标位于 `0..1000`，Detection 通常返回矩形，Segmentation 可返回多点多边形。
4. 识别阶段按布局块从原图裁剪，必要时旋转、补白或放大。
5. 不同块类型使用不同 prompt 和采样参数。
6. 表格内容为 OTSL，公式内容为 LaTeX，普通内容为文本。
7. `image`、`list`、`equation_block` 不直接执行第二阶段识别。
8. 布局输出顺序就是上游后续使用的阅读顺序。

主要协议依据：

- `opensource/NaviDC-OCR/NaviOCR/vlm_utils/NaviOCR_client.py`
- `opensource/NaviDC-OCR/NaviOCR/vlm_utils/structs.py`
- `opensource/NaviDC-OCR/NaviOCR/src/vlm_analyze.py`
- `opensource/NaviDC-OCR/NaviOCR/src/vlm_magic_model.py`
- `opensource/NaviDC-OCR/NaviOCR/vlm_utils/post_process/`

### 2.1 已知上游缺口

上游 `new_vlm_client()` 声明支持 `http-client`，但当前仓库没有提交对应的
`vlm_client/http_client.py`，README 也只正式展示进程内 `vllm-engine` 和
`vllm-async-engine`。因此不能把上游 HTTP 请求实现视为已确认契约。

实施前必须用真实 NaviDC-OCR vLLM 服务确认：

- 标准 `/v1/chat/completions` 是否可直接使用；
- multimodal message 中图片和文本的顺序；
- served model name；
- `top_k`、`repetition_penalty`、`no_repeat_ngram_size` 的传递方式；
- 是否需要 `skip_special_tokens=false`；
- 布局和识别阶段所需的最大输出 token 数。

如果标准 OpenAI endpoint 无法保持上游行为，则增加一个独立轻量 sidecar service，
由 sidecar 封装上游推理客户端；不在 Rust 适配器中嵌入 Python 运行时。

## 3. 目标协议定义

协议名使用 `navidc-ocr`，建议的声明如下：

| 字段 | 值 |
| --- | --- |
| `mode` | `ModelProtocol` |
| `shape` | `LayoutThenRecognize` |
| `transport` | `OpenAiChatCompletions` |
| `preprocess` | `HardResize` |
| `decode` | 新增 `NaviLayoutTokens` |
| `coordinates` | `Norm0To1000` |
| `order` | `FromModel` |
| 默认 endpoint | `http://localhost:8000/v1/chat/completions` |
| 默认 model | `StarDoc-AI/NaviDC-OCR` |
| model stage | 单一远程 `vlm` stage |
| endpoint 环境变量元数据 | `NAVIDC_OCR_ENDPOINT` |

`ProtocolAdapter` 能力声明：

- `coordinate_system()` 返回 `Norm0To1000`；
- `provides_reading_order()` 返回 `true`；
- `raw_output_format()` 返回新的 NaviDC 布局格式；
- `emitted_signals()` 第一版不声明 span、font size 或 merge hint；
- block source 使用 `LayoutThenRecognize`。

## 4. 分阶段实施

### 阶段 A：建立协议 fixture 和真实接口门槛

先保存来自上游或真实服务的脱敏 fixture：

- Detection 的四坐标矩形；
- Segmentation 的八坐标及更多点多边形；
- `up`、`right`、`down`、`left` 四种方向；
- NaviDC 全量原始类别；
- 空行、噪声前后缀、缺失方向、奇数坐标、越界坐标、截断行；
- text、table、formula、code、seal、char 的第二阶段输出；
- `finish_reason=length` 响应。

随后对真实 vLLM 服务做单页探测，记录实际请求体和响应。只有标准
Chat Completions 契约验证通过后，才进入 adapter 主体实现。

交付物：fixture、请求契约说明、可复现的单页 smoke 命令。

### 阶段 B：注册协议与公共入口

修改：

- `uparser/crates/uparser-core/src/protocol_spec.rs`
- `uparser/crates/uparser-core/src/adapters/mod.rs`
- `uparser/crates/uparser-core/src/runner.rs`
- `uparser/crates/uparser-core/src/cli.rs` 中的协议帮助文本和枚举测试

工作项：

1. 增加 `pub mod navidc_ocr`。
2. 将 `navidc-ocr` 加入 built-in registry。
3. 在 `canonical_protocol()` 中接受新名称。
4. 增加 `ProtocolSpec`，并使 `uparser protocols` 输出完整能力。
5. 通过现有 `AdapterOverrides` 支持 `--endpoint` 与 `--model`。
6. 让 `doctor navidc-ocr` 使用协议默认 endpoint 或配置解析结果。
7. 保持 Node/Python 公共 options 的字符串协议接口不变，避免新增枚举 ABI。
8. 不把该协议加入 `auto` 候选，等待质量评测后单独决策。

### 阶段 C：实现 NaviDC 布局解析

在 `output_parse.rs` 增加专用结构，例如：

```rust
struct NavidcLayoutBlock {
    geometry_1000: Geometry,
    category_raw: String,
    angle: Option<u32>,
}
```

解析规则：

1. 严格解析标准 `<box:...><label:...><...>` 行。
2. 支持有限的首尾空白和无害噪声恢复。
3. 坐标必须是偶数个，且至少有四个。
4. 正常范围是 `0..1000`；轻微越界执行 clamp 并记录 warning。
5. 四个数生成 `Geometry::Rect`。
6. 多于四个数生成 `Geometry::Polygon`。
7. 多边形的 `bbox_px` 使用所有顶点的外接矩形。
8. 从尾部 token 解析 `up/right/down/left`。
9. 未知方向保留为 `None` 并告警，不丢弃有效块。
10. 无效单行按块跳过，不让单个坏块导致整页失败。
11. 返回顺序与模型行顺序一致，并用于 `reading_order`。

不复用 MinerU custom-token parser，因为两者的标签语法、旋转标记和多边形能力不同。

### 阶段 D：补齐图像处理

在 `imaging.rs` 复用或新增以下能力：

- 页面硬缩放到 `1036x1036`；
- `0..1000` 矩形和多边形映射到原始页面像素；
- 映射后的坐标交换、clamp 和最小一像素尺寸保护；
- 矩形裁剪；
- 多边形 mask 裁剪，区域外填白；
- 按 `90/180/270` 度旋转；
- 长宽比超过 `50:1` 时居中补白；
- 短边小于 28 像素时 bicubic 放大；
- 总像素超过上游 `MAX_PIXELS` 时等比缩小；
- PNG 编码和 data URL 生成；
- image block 的原始区域资产保存。

多边形 mask 是 Segmentation 模式与拍照文档适配的必要条件，不能只取外接矩形后直接识别，
否则会把弯曲区域之外的邻近内容一起送入模型。

当前 `imaging.rs` 存在用户未提交修改，实施时必须基于当前工作树增量合并，不覆盖已有变更。

### 阶段 E：实现 `NavidcOcrAdapter`

新增 `uparser/crates/uparser-core/src/adapters/navidc_ocr.rs`。

页面执行流程：

1. 解码 rasterized page 并转换为 RGB。
2. 创建 `1036x1036` 布局图。
3. 构造布局请求并通过 `shape_executor::chat_stage` 调度。
4. 提取 chat content，解析布局块并收集 warning。
5. 将坐标映射回原始页面，完成类别归一化和结构预处理。
6. 对需要内容识别的块生成裁剪图。
7. CPU 图像处理完成后再获取共享网络 permit。
8. 并发发送第二阶段请求。
9. 按原始 index 收集响应，保证异步完成顺序不改变阅读顺序。
10. 将内容转换成统一 `Block`。
11. 单块识别失败写入该块的 `error`，其他块继续返回。

请求使用上游 system prompt：

```text
You are a helpful assistant.
```

第二阶段 prompt：

| 原始类型 | Prompt | 处理结果 |
| --- | --- | --- |
| `text` 及默认文本类 | `Please output the text content from the image.` | `Block.text` |
| `table` | `This is the image of a table. Please output the table in OTSL format.` | OTSL 转 `Block.html` |
| `formula` | `Please write out the expression of the formula in the image using LaTeX format.` | 修复后写入 `Block.latex` |
| `code` | `The image contains a code snippet, please output the parsing result.` | `Block.text` |
| `seal` | `Seal Recognition:` | `Block.text` |
| `char` | `This is a scientific figure. Please extract the table implied by this figure.` | 保留原始类别并输出文本 |

跳过第二阶段的类型：`image`、`list`、`equation_block`。

采样参数以真实 endpoint 验证结果为准，基线保持上游语义：temperature 0、top-p 0.01、
top-k 1、repetition penalty 1.0；识别阶段按类别设置 presence/frequency penalty。

### 阶段 F：类别与结构归一化

在 `category_map.rs` 增加：

- `NAVIDC_OCR_CATEGORIES`
- `map_navidc_ocr_category()`
- 类别归一化和未知类别 warning 测试

建议映射：

| NaviDC 原始类别 | 统一类别 |
| --- | --- |
| `title` | `title` |
| `text`, `aside_text`, `phonetic` | `text` |
| `table` | `table` |
| `image` | `image` |
| `equation` | `equation` |
| `code`, `algorithm` | `code` |
| `list` | `list` |
| `ref_text` | `reference` |
| `table_caption`, `image_caption`, `code_caption` | `caption` |
| `table_footnote`, `image_footnote`, `page_footnote` | `footnote` |
| `header`, `footer`, `page_number` | 对应页面附属类别 |
| `seal` | `text`，保留 `category_raw=seal` |
| `char` | `text`，保留 `category_raw=char` |
| `unknown` 或新类别 | `unknown` 并记录 warning |

必须实现上游关键结构规则，而不只是平铺所有 layout blocks：

- `equation_block` 是容器，不输出无内容的空块；
- `list` 容器内覆盖率达到阈值的 text/ref_text 块转换为 list item，空 list 容器丢弃；
- image/table/code caption 和 footnote 保持与主体相邻的模型顺序；
- image block 保留裁剪资产；
- caption 继续作为普通阅读顺序块，同时允许现有 asset-caption 关联逻辑挂接；
- 不用几何阅读顺序覆盖模型 index。

第一版不完整复制上游 `MagicModel` 的 Python 层级对象。统一 IR 当前没有等价的任意父子层级，
因此只移植会影响最终 Markdown 正确性的关系规则，保持数据不丢失。

### 阶段 G：内容后处理与容错

复用现有共享能力：

- `otsl.rs`：OTSL 转 HTML；
- `formula_repair.rs`：公式清理、括号修复和 display math 包装；
- `robustness.rs`：普通文本重复生成检测和升温重试；
- `assets.rs`：图片区域写出；
- `postprocess.rs` 和 `render/`：统一后处理与输出。

协议特定规则：

- text 规范化成对的单美元符号，保留行内公式；
- formula 去除重复外围定界符后统一包装；
- table/formula 不使用普通文本重复度修剪；
- OTSL 失败时保留错误和可诊断原始内容，不静默生成空表；
- `finish_reason=length` 记录阶段级 warning；
- 布局请求失败仍是页面级错误；单个 recognition 请求失败只是 block 级错误；
- 不把网络错误伪装成空识别结果。

上游 OTSL 转换器与本项目实现必须使用相同 fixture 做差异测试。若存在真实格式差异，优先扩展
共享 `otsl.rs`，但需要确保 MinerU 和 MonkeyOCR 回归测试仍通过。

## 5. 配置策略

基础配置继续使用现有优先级：

```text
CLI flag -> environment -> config[navidc-ocr] -> adapter default
```

示例：

```toml
[navidc-ocr]
endpoint = "http://127.0.0.1:8000/v1/chat/completions"
model = "StarDoc-AI/NaviDC-OCR"
```

布局模式处理建议：

- 第一版默认 `Detection`，与上游默认值一致；
- parser 和多边形裁剪从第一版就兼容 `Segmentation` 输出；
- 在真实服务确认切换方式前，不仓促增加公共 CLI 参数；
- 如果切换只依赖 prompt，后续增加受限配置 `layout_mode = detection|segmentation`，并纳入 cache fingerprint；
- 不用模型名称或 endpoint 字符串隐式推断布局模式。

## 6. 测试计划

### 6.1 单元测试

- adapter 名称、能力和 model stage；
- 默认 endpoint/model 与 `ProtocolSpec` 一致；
- layout 请求 messages、图片尺寸、prompt 和 sampling；
- 四坐标矩形解析；
- 多点多边形解析；
- 四种旋转方向；
- 坐标缩放、反转、越界 clamp 和退化框；
- 多边形 mask 裁剪区域外为白色；
- 全类别映射；
- list 容器折叠；
- image 资产保留；
- OTSL、公式和行内公式后处理；
- 噪声行、截断行、未知类别恢复；
- recognition 单块失败不影响同页其他块；
- warning 经 `ParseCtx` 汇总。

### 6.2 Adapter 离线集成测试

使用 `MockDispatch` 验证：

- 一次 layout 加 N 次 recognition 的完整编排；
- skip 类型没有多余请求；
- 并发响应乱序后仍按 layout index 输出；
- 每次网络请求正确使用共享 semaphore；
- 等价的 NaviDC、MinerU、MonkeyOCR fixture 生成一致的核心 IR/Markdown；
- table、formula、image、caption 的渲染结果符合统一契约。

### 6.3 CLI 集成测试

- `uparser protocols` 包含 `navidc-ocr`；
- `uparser doctor navidc-ocr` 使用正确 endpoint；
- `parse --mode protocol --protocol navidc-ocr` 可执行；
- 未配置 endpoint、不可达 endpoint 和未知协议错误正确；
- `--endpoint`、`--model` 覆盖配置；
- `--pages`、stream、Markdown、JSON、document-json；
- 图片资产目录与 `--no-assets`；
- cache 按 protocol、endpoint、model、layout mode 隔离。

### 6.4 真实模型验收

至少覆盖：

- 原生数字 PDF；
- 扫描 PDF；
- PNG/JPEG；
- 手机拍照和透视变形文档；
- 多栏文档；
- 复杂合并单元格表格；
- 行内和独立公式；
- 旋转文本；
- Detection 和 Segmentation 多边形页面；
- 多页并发、页面局部失败和服务端限流。

使用相同输入比较上游 `infer.py` 和 `uparser --protocol navidc-ocr`：

- 逐页 block 数量与顺序；
- 文本 edit distance；
- 表格 TEDS；
- 公式指标；
- Markdown 结构；
- 图片、caption 和 footnote 是否丢失。

## 7. 验证命令

实现后执行：

```bash
cd uparser
cargo fmt --all --check
cargo test -p uparser-core
cargo clippy -p uparser-core --all-targets -- -D warnings
cargo test --workspace
```

真实服务 smoke 示例最终以阶段 A 验证后的部署命令为准，客户端形式预计为：

```bash
uparser doctor navidc-ocr \
  --endpoint http://127.0.0.1:8000/v1/chat/completions

uparser parse document.pdf \
  --mode protocol \
  --protocol navidc-ocr \
  --endpoint http://127.0.0.1:8000/v1/chat/completions \
  --model StarDoc-AI/NaviDC-OCR \
  --format markdown
```

## 8. 文档更新

更新以下文档中的协议数量、能力矩阵、配置和示例：

- `README.md`
- `UPARSER_GUIDE.md`
- `skills/uparser/references/protocols.md`
- `skills/uparser/references/cli.md`
- `skills/uparser/references/config.example.toml`
- 必要时更新 `skills/uparser/SKILL.md` 的协议选择建议

`skills/uparser/SKILL.md` 当前存在未提交修改，实施时只做定点增量编辑。

## 9. 风险与控制

| 风险 | 控制措施 |
| --- | --- |
| 上游 HTTP 客户端缺失 | 先做真实 endpoint 合同验证；失败时采用 sidecar |
| 服务不接受上游采样扩展字段 | 逐字段探测，区分标准字段和 `vllm_xargs` |
| Segmentation 多边形被矩形化后污染识别 | 实现 polygon mask crop 并做像素级测试 |
| list/container 平铺导致重复或空输出 | 在 recognition 前执行包含关系和容器折叠 |
| OTSL 方言差异 | 用真实输出 fixture 对比两套转换器 |
| 模型输出截断 | 检查 `finish_reason`、有限恢复并记录 warning |
| 两层并发死锁 | 图像处理后才获取共享 permit；增加嵌套并发测试 |
| 新协议影响 auto 行为 | 第一版仅显式使用，benchmark 后再调整路由 |
| 用户工作树已有相关修改 | 基于当前文件增量合并，不还原或覆盖现有变更 |

## 10. 完成标准

只有同时满足以下条件才视为适配完成：

1. `navidc-ocr` 能从 CLI、Rust API、Node/Python 的字符串协议入口调用。
2. Detection 和 Segmentation 布局输出都能解析，矩形和多边形坐标正确回映。
3. text、table、formula、code、image、list 等关键类别进入正确统一 IR 字段。
4. 单块失败不丢失整页，布局失败形成明确页面错误。
5. Markdown、JSON、document-json 和图片资产路径均可用。
6. registry、协议清单、doctor、配置解析和 cache 行为有自动化测试。
7. `cargo fmt`、核心 crate 测试、clippy 和 workspace 测试通过。
8. 至少一次真实 NaviDC-OCR 服务端到端 smoke 通过。
9. 与上游对相同样本的结构差异已记录，关键质量指标无不可解释退化。
10. 用户文档包含经过实际验证的服务启动和调用命令。

## 11. 推荐提交拆分

为了便于审阅和回退，建议拆成以下提交：

1. `test: add navidc ocr protocol fixtures`
2. `feat: add navidc layout parser and geometry helpers`
3. `feat: add navidc ocr protocol adapter`
4. `test: cover navidc cli and cross-protocol contracts`
5. `docs: document navidc ocr deployment and usage`

