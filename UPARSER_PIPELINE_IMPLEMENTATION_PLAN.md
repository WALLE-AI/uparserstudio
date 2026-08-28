# uparser Pipeline 多模型实现方案

## 1. 目标与结论

基于 `/home/dataset1/gaojing/magic-pdf.json` 中配置的模型，构建可由 uparser V2 StageGraph 编排的传统文档解析 pipeline。架构、批处理、后处理和端到端行为以 `opensource/MinerU` 当前版本为参考；旧版 `magic_pdf` 只用于兼容现有权重的模型加载和阶段级预后处理校验：

- DocLayout-YOLO：版面检测。
- YOLOv8-MFD：整页公式检测。
- PP-FormulaNet-plus-M：默认公式识别；UniMERNet-small 保留为对照 profile。
- PaddleOCR：文本检测与识别。
- RapidTable/SLANet-plus：表格结构识别。
- LayoutReader 或 MinerU `para_split`：阅读顺序与段落重建。

推荐采用“Rust 负责流程编排，Python 模型服务负责推理”的架构。第一阶段先将 legacy 模型输出适配为新版 MinerU pipeline 的标准中间结果，再对齐新版 MinerU 的文档级行为，之后才进行 ONNX、TensorRT 或进程内推理优化。

当前默认参考源码已升级为 MinerU `3.4.5`、commit
`4fe4bde114a23ee5dd637eae99b767f4669bf58c`，模型清单为
`pipeline/model-manifest-mineru-3.4.5.json`。旧版 3.4.4/`magic-pdf.json` 清单仅保留用于严格同模
A/B 和阶段诊断。后续升级必须显式更新 manifest、golden 和评测报告，不能隐式跟随工作区变化。

3.4.5 默认链路为 PP-DocLayoutV2 + PP-OCRv6 + PP-FormulaNet-plus-M + 表格分类/方向 +
UnetStructure/SLANet-plus，并直接复用官方文档 finalize 与 Markdown。1,651 页候选实验证明其
直接候选的 OmniDocBench Overall 为 `88.5123`、Formula CDM 为 `88.5788`；最终 uparser 服务路径
为 `88.4489`/`88.4867`，分别高于 UniMERNet-small 的 `85.3391`/`79.5826`，因此已切为默认；
设置 `MINERU_FORMULA_CH_SUPPORT=False` 可复现 UniMERNet
对照。

不建议第一阶段直接在 Rust 中重写全部模型推理。当前模型横跨 PyTorch、Paddle、Transformers 和 ONNX，模型精度高度依赖上游的图像预处理、批处理、输出解码和后处理逻辑。

## 2. 当前配置和资产

配置文件：`/home/dataset1/gaojing/magic-pdf.json`

主要配置：

```json
{
  "models-dir": "/home/dataset1/gaojing/llm/uRAG/mineru-models/PDF-Extract-Kit-1.0/models",
  "layoutreader-model-dir": "/home/dataset1/gaojing/llm/uRAG/mineru-models/layoutreader",
  "device-mode": "cuda",
  "layout-config": { "model": "doclayout_yolo" },
  "formula-config": {
    "mfd_model": "yolo_v8_mfd",
    "mfr_model": "unimernet_small",
    "enable": true
  },
  "table-config": {
    "model": "rapid_table",
    "sub_model": "slanet_plus",
    "enable": true,
    "max_time": 400
  }
}
```

已确认模型目录包含 DocLayout-YOLO、YOLOv8-MFD、UniMERNet、PaddleOCR 和部分表格模型资产。`models-dir` 内没有 `TabRec/SlanetPlus/slanet-plus.onnx`，但当前 legacy `magic_pdf` runtime 在 `resources/slanet_plus/slanet-plus.onnx` 提供了对应资产；manifest 必须固定该运行时文件的来源和 hash，后续不能依赖隐式包资源。

## 3. uparser 当前差距

当前实现位于：

- `uparser/crates/uparser-core/src/adapters/pipeline.rs`
- `uparser/crates/uparser-core/src/adapters/pipeline_serving.rs`
- `uparser/crates/uparser-core/src/stage_graph.rs`

主要差距：

1. layout、OCR、formula 依赖尚不存在的 `localhost:9001` 模型服务。
2. OCR 接口只返回一个文本字符串，无法保留文本行、span、四边形和置信度。
3. 本地表格实现只运行原始 ONNX tensor，没有 SLANet 解码和单元格匹配。
4. StageGraph 没有独立的整页 MFD 节点，formula 只能处理 layout 返回的 equation 区域。
5. table 节点声明使用内部 OCR，无法消费 PaddleOCR 的文字和坐标结果。
6. 阅读顺序使用几何排序，没有接入 LayoutReader 或 MinerU `para_split`。
7. 当前请求按区域逐个发送 base64 PNG，不适合高吞吐 GPU 批处理。

## 4. 目标架构

```text
PDF / Image
     |
Render + coordinate transform
     |
     +--------------------+
     |                    |
DocLayout-YOLO       YOLOv8-MFD
     |                    |
     |              UniMERNet-small
     |                    |
PaddleOCR Det <----- formula masking
     |
PaddleOCR Rec -----------+
     |                    |
RapidTable / SLANet-plus |
     |                    |
     +------ merge -------+
              |
 LayoutReader / para_split
              |
      uparser Document IR
              |
     Markdown / JSON / HTML
```

建议新增 Python 服务目录：

```text
services/pipeline-model-server/
  pyproject.toml
  src/uparser_pipeline_server/
    app.py
    config.py
    schemas.py
    model_registry.py
    preprocess.py
    postprocess.py
    stages/
      layout.py
      mfd.py
      ocr.py
      mfr.py
      table.py
      reading_order.py
  tests/
```

职责边界：

- Python 服务复用或兼容新版 MinerU runtime，负责线程安全模型单例、CUDA 管理、预处理、推理、后处理和批处理。
- Rust 负责 StageGraph、任务编排、超时重试、区域错误隔离、坐标统一、Document IR 和输出渲染。
- `magic-pdf.json` 只作为兼容输入，运行时转换为 uparser 自有的强类型 pipeline profile。

## 5. 目标执行流程

### 5.1 页面渲染

- 固定渲染 DPI 和颜色空间，记录 PDF point 到 render pixel 的变换矩阵。
- 所有模型输出先保留 render pixel 坐标，最终统一映射回 page coordinate。
- 页面 ID、原始尺寸、渲染尺寸和旋转信息贯穿所有阶段。

### 5.2 Layout 和 MFD

- DocLayout-YOLO 与 YOLOv8-MFD 对整页并行执行，并合并成新版 MinerU `BatchAnalyze` 使用的 `label/bbox/score` 结构。
- DocLayout-YOLO 首先保持参考实现参数：`imgsz=1280`、`conf=0.10`、`iou=0.45`。
- MFD 输出 `inline_formula` 和 `display_formula` 区域，不依赖 layout 是否产生 equation 类别。
- 对 layout 与 MFD 框执行重叠消歧、包含关系处理和类别归一化。

### 5.3 OCR 和公式识别

- 在 OCR 检测前遮罩公式区域，避免公式被重复识别为普通文本。
- PaddleOCR 先检测文本四边形，再做旋转裁剪和宽度分桶识别。
- UniMERNet 按公式裁剪尺寸分桶并批量识别。
- 行内公式按坐标插回文本行；行间公式生成独立 block。

### 5.4 表格识别

- 按新版 MinerU 执行有线/无线表格分类、方向分类和旋转校正。
- table 阶段同时接收表格裁剪图、OCR boxes 和 OCR text。
- SLANet-plus 输出结构 token，由 PaddleOCR table matcher 完成文字到单元格的匹配。
- 输出 canonical HTML，同时保留 cell bbox、rowspan、colspan、文本和置信度。
- 表格内部公式和图片按新版 MinerU 作为首版验收能力，不作为后续可选增强。

### 5.5 阅读顺序与内容组装

- 内部部署可以接入现有 LayoutReader 权重。
- 可分发版本使用 MinerU `para_split` 和确定性几何排序作为后备。
- 文档级 finalize 对齐新版 MinerU：post-OCR、公式编号优化、`para_split`、跨页表格合并和标题层级处理。
- 合并标题、段落、列表、脚注、页眉页脚、跨栏内容和跨页段落。
- 结果进入 uparser Document IR，再由现有 Markdown/JSON 渲染器输出。

## 6. StageGraph 改造

新增或调整 StageKind：

```text
Preprocess
Layout
FormulaDetect
OcrDetect
OcrRecognize
FormulaRecognize
Table
Assemble
Order
```

新增 StageData：

```text
PageImage
LayoutRegions
FormulaRegions
TextRegions
OcrSpans
FormulaSpans
TableRegions
RecognizedRegions
OrderedBlocks
```

推荐依赖关系：

```text
preprocess -> layout
preprocess -> formula_detect
layout + formula_detect -> ocr_detect
ocr_detect -> ocr_recognize
formula_detect -> formula_recognize
layout + ocr_recognize -> table
layout + ocr_recognize + formula_recognize + table -> assemble
assemble -> order
```

失败策略：

- 页面渲染、layout 失败：`AbortPage`。
- MFD、OCR、MFR、table 的单区域失败：`IsolateRegion`。
- 可选的 reading-order 模型失败：退化到确定性排序，并输出 warning。
- 禁止远程阶段失败后静默切换到不同模型，避免结果不可复现。

## 7. Pipeline V2 服务协议

建议接口同时提供阶段级诊断接口和新版 MinerU 风格的页级融合接口：

```text
GET  /health
GET  /v2/models
POST /v2/pipeline/layout:batch
POST /v2/pipeline/mfd:batch
POST /v2/pipeline/ocr:batch
POST /v2/pipeline/mfr:batch
POST /v2/pipeline/table:batch
POST /v2/pipeline/pages:analyze
```

协议要求：

- 所有输入输出包含 `request_id`、`page_id`、`region_id` 和 schema version。
- region 包含 polygon、bbox、label、confidence 和坐标系。
- OCR 返回逐 line/span 的四边形、文本、语言和置信度。
- MFD 返回行内/行间类别；MFR 按 `region_id` 返回 LaTeX。
- table 返回 HTML、cells、结构 token、cell bbox 和置信度，不强制转成 OTSL。
- 响应包含 model name、revision、权重 hash、耗时和 warning。
- `pages:analyze` 返回 normalized model output；阶段接口主要用于诊断和精度对齐。
- 首版可使用 JSON + base64；性能阶段升级为 multipart/binary batch，避免重复编码。
- 长文档按新版 MinerU processing window 执行，默认窗口 64 页并支持完成文档流式回调或落盘。

## 8. 配置设计

新增 uparser pipeline profile，例如：

```toml
[pipeline]
schema_version = 2
device = "cuda:0"
server = "http://127.0.0.1:9001"

[pipeline.layout]
model = "doclayout_yolo"
weights = "/path/to/doclayout_yolo.pt"
batch_size = 4

[pipeline.formula_detection]
model = "yolo_v8_mfd"
weights = "/path/to/yolo_v8_ft.pt"

[pipeline.formula_recognition]
model = "unimernet_small"
weights = "/path/to/unimernet_small"

[pipeline.ocr]
model = "paddleocr_torch"
lang = "ch"

[pipeline.table]
model = "rapid_table"
sub_model = "slanet_plus"
weights = "/path/to/slanet-plus.onnx"
max_time_seconds = 400

[pipeline.reading_order]
backend = "layoutreader"
weights = "/path/to/layoutreader"
fallback = "para_split"
```

提供只读转换命令：

```bash
uparser pipeline config import-magic-pdf \
  --input /home/dataset1/gaojing/magic-pdf.json \
  --output pipeline.toml
```

启动前执行严格 preflight，检查文件、hash、格式、CUDA、依赖版本、OCR 字典、许可证标记和服务 schema。

## 9. 分阶段实施计划

| 阶段 | 工作内容 | 交付物与验收 | 预计 |
|---|---|---|---:|
| P0 资产冻结 | 模型路径、格式、大小、SHA256、来源、许可证；补齐 SLANet-plus 和字典 | `model-manifest.json`；preflight 能准确报告缺失项 | 1–2 天 |
| P1 参考基线 | 冻结新版 MinerU commit/version；旧环境只生成 legacy 模型阶段 oracle；选择 30–50 页覆盖集 | 新版 MinerU 端到端 oracle + legacy 权重阶段 oracle | 2–3 天 |
| P2 服务骨架 | Python 服务、配置、健康检查、模型 registry、CUDA 生命周期、日志 | 服务启动后模型只加载一次；基础 contract tests 通过 | 2–3 天 |
| P3 Layout + MFD | DocLayout-YOLO、YOLOv8-MFD、类别和坐标映射、框合并 | 与 oracle 高度一致；Rust StageGraph 能消费两路输出 | 4–6 天 |
| P4 OCR + MFR | 公式遮罩、OCR det/rec、旋转裁剪、分桶、UniMERNet 批处理 | line/span 和公式正确回填；阶段 golden tests 通过 | 5–8 天 |
| P5 表格 | SLANet-plus、table matcher、HTML/cell 输出 | rowspan/colspan/cell 文本正确；移除占位 ONNX 输出 | 5–8 天 |
| P6 阅读顺序 | 对齐新版 MinerU finalize 链路；LayoutReader 作为兼容可选 backend | 多栏、标题、列表、跨页段落和跨页表格测试通过 | 4–6 天 |
| P7 uparser 集成 | V2 schema、StageGraph、配置、CLI、错误隔离和 IR 映射 | `uparser parse --protocol pipeline` 端到端可运行 | 3–5 天 |
| P8 性能优化 | 页级批处理、宽度分桶、动态 batch、流式页面、显存水位 | 无逐区域请求爆炸；形成 P50/P95、吞吐和显存报告 | 4–7 天 |
| P9 评测发布 | OmniDocBench/OpenDataLoader Bench、回归和故障注入 | 评测报告、回归门禁、部署文档和复现命令 | 3–5 天 |

单人预计 5–7 周；两人按“Python 模型服务”和“Rust 编排及评测”拆分，预计约 3–4 周。

## 10. 测试与评测门禁

### 10.1 单阶段测试

- Layout/MFD：类别一致率、box IoU、漏检率、误检率。
- OCR：检测 precision/recall、标准化文本准确率、置信度分布。
- MFR：标准化 LaTeX exact match、编辑距离、行内/行间分类。
- Table：TEDS、结构准确率、cell 文本准确率、rowspan/colspan。
- Reading order：block 顺序 edit distance 和段落合并准确率。

### 10.2 一致性门禁

- 相同权重 FP32 推理相对 frozen oracle，框匹配 IoU 目标不低于 0.99。
- 标准化 OCR/MFR 输出与 oracle 的一致率目标不低于 99%。
- 非确定性算子必须记录 seed、CUDA、Torch/Paddle 和驱动版本。

### 10.3 端到端门禁

- 使用 OmniDocBench 分别报告 text、formula、table、reading-order 和 overall。
- 使用 OpenDataLoader Bench 报告官方指标及文档类别分布。
- 每个关键指标相对参考 pipeline 下降超过 1 个百分点即阻断发布。
- 同时与当前 uparser native、native+OCR 和 MinerU/VLM 结果并列比较。
- 回归集必须覆盖扫描 PDF、原生 PDF、中英文混排、多栏、旋转文本、行内公式、无线表格和跨页内容。

### 10.4 性能门禁

- 冷启动时间和模型加载时间。
- 单页延迟、32 页 warm batch 吞吐、P50/P95。
- 峰值 GPU 显存、CPU 内存和临时磁盘占用。
- 超长文档流式处理和 backpressure。
- 单区域失败、服务超时、CUDA OOM 和服务重启后的恢复行为。

## 11. 优化顺序

1. 首先完成原模型 Python 推理和结果对齐。
2. 第二步消除逐区域 HTTP 请求，加入批量接口和分桶。
3. 第三步分析 profiler，优化图片编码、数据复制和 GPU pipeline。
4. 只有在 golden tests 完整后，才尝试导出 ONNX/TensorRT。
5. 每替换一个 runtime，只比较该阶段及端到端结果，不允许一次替换全部模型。

## 12. 许可证与发布策略

当前 MinerU 已明确移除两个 AGPLv3 模型 DocLayout-YOLO、YOLOv8-MFD，以及 CC-BY-NC-SA 4.0 的 LayoutReader。因此发布前需要正式的许可证审查，本方案不构成法律意见。

建议维护两个 profile：

- `legacy-byom`：内部部署或用户自行提供模型，使用当前 `magic-pdf.json` 对应资产。
- `redistributable`：使用当前 MinerU 的 PP-DocLayoutV2、兼容公式检测能力和 `para_split`，不随 uparser 分发受限权重。

代码、模型权重、容器镜像和下载脚本必须分别做许可证检查，不能因为代码采用宽松许可证就默认模型可以重新分发。

## 13. 关键风险

| 风险 | 影响 | 应对措施 |
|---|---|---|
| SLANet-plus 权重缺失 | 表格阶段无法落地 | P0 补齐并冻结 hash；未补齐前禁止以 table-ready 标记发布 |
| 旧模型代码与当前 MinerU 不一致 | 预后处理产生精度偏差 | P1 固定原始 oracle，逐阶段对齐，不直接套用新版默认参数 |
| OCR 语言配置缺失 | 多语言准确率不可控 | profile 显式配置语言，并记录实际 det/rec 模型 revision |
| base64 逐 crop 请求 | 吞吐低、CPU 开销高 | 首版验证后升级 batch multipart/binary 接口 |
| LayoutReader 许可证受限 | 无法公开分发 | BYOM profile + `para_split` fallback |
| ONNX 转换精度漂移 | 阶段指标下降 | 转换必须受 golden 和端到端双重门禁约束 |
| GPU OOM | 长文档失败 | 动态 batch、显存水位、流式页面和单页重试降级 |

## 14. 里程碑与完成定义

### M1：模型服务最小闭环

完成 P0–P4，30–50 页 golden 集上 layout、MFD、OCR 和 MFR 与旧 pipeline 对齐。

### M2：完整 pipeline

完成 P5–P7，表格、阅读顺序、uparser IR 和 Markdown 输出端到端可用。

### M3：性能与发布

完成 P8–P9，所有准确率、性能、错误恢复和许可证门禁通过。

最终完成条件：

- 配置中的所有启用模型均由真实推理实现，不存在占位输出。
- 模型和服务启动前可以执行严格 preflight。
- 端到端结果可复现，响应携带模型 revision/hash。
- OmniDocBench 和 OpenDataLoader Bench 报告包含各子项与基线差异。
- 关键准确率指标没有超过门禁的回退。
- 长文档、并发、OOM 和阶段失败具有明确的恢复或报错行为。
- 发布物完成代码和模型的许可证检查。
