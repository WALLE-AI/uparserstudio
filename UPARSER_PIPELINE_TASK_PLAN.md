# uparser Pipeline Task 级迭代研发计划

## 1. 计划范围

本计划是 `UPARSER_PIPELINE_IMPLEMENTATION_PLAN.md` 的任务级拆分。默认生产 profile 已更新为
MinerU 3.4.5、commit `4fe4bde114a23ee5dd637eae99b767f4669bf58c` 和
PP-DocLayoutV2 完整模型链；`/home/dataset1/gaojing/magic-pdf.json` 的 legacy profile 仅用于严格
同模 A/B、权重兼容和阶段级校验。

目标模型链路：

```text
PP-DocLayoutV2
        -> PP-OCRv6 + PP-FormulaNet-plus-M（UniMERNet-small 对照）
        -> table cls/orientation + UnetStructure/SLANet-plus
        -> MinerU 3.4.5 finalize/para_split/cross-page merge
        -> authoritative Markdown + uparser Document IR
```

任务规模约束：

- 单任务预计 0.5–3 人日。
- 每个任务必须有独立产物和自动化验收方式。
- 模型转换和性能优化不能先于参考结果冻结。
- 未通过当前迭代 exit gate，不进入依赖该结果的后续迭代。

建议角色：

- `ML`：模型加载、预处理、推理、后处理、精度对齐。
- `CORE`：Rust StageGraph、协议、配置、IR 和渲染集成。
- `EVAL`：数据集、golden、指标、报告和回归门禁。
- `DEVOPS`：环境、镜像、GPU、服务和可观测性。

状态约定：`TODO`、`DOING`、`BLOCKED`、`DONE`。

### 2026-08-26 执行快照

| 范围 | 状态 | 已落地产物 / 剩余门禁 |
|---|---|---|
| I0 | DONE | legacy 与 3.4.5 双 manifest、许可证矩阵、golden corpus 和 GPU 真实模型 smoke 已完成；关键 3.4.5 权重 hash 已冻结 |
| I1 | DONE | Rust/Python V2 schema、StageGraph、OpenAPI、跨语言 fixture 均通过 |
| I2 | DOING | FastAPI、线程安全 registry、原生 batch、失败逐项降级、配置化启动与 health 已完成；超时、取消、日志、显存指标待补 |
| I3 | DOING | DocLayout-YOLO、YOLOv8-MFD 真实后端与 CPU smoke 完成；40 页精度 oracle 待跑 |
| I4 | DOING | PP-OCRv6、PP-FormulaNet-plus-M、公式遮罩/回填和真实 smoke 完成；更细粒度尺寸分桶待补 |
| I5 | DONE | latest profile 已接入表格分类/方向、UnetStructure wired 与 SLANet-plus wireless 官方链路 |
| I6 | DONE | latest profile 直接复用 3.4.5 post-OCR、公式编号、para_split、跨页表格合并、标题层级和官方 Markdown |
| I7 | DONE | Rust `pipeline` registry 已切到 V2；layout/MFD、OCR/MFR、table、assemble、reading-order 全流程由 `pipeline_v2.rs` 编排，模型仅通过服务调用 |
| I8 | DOING | MinerU processing-window 原生 batch 已完成；metrics、OOM 降 batch 和并发压测待补 |
| I9 | DOING | 服务内官方 finalize 路径评测已通过；新 Rust 分阶段工作流需重跑 ODL/Omni，许可证签核、持续回归和发布评审待完成 |

当前真实表格页 CPU oracle：20 regions、40 OCR spans、6 formula spans、1 table/60 cells，整页 95.645 秒；输出见 `pipeline/oracle/page-pipeline-table-cpu.json`。

## 2. 迭代总览

| 迭代 | 目标 | 主要交付 | 建议周期 |
|---|---|---|---:|
| I0 | 资产、环境和基线冻结 | manifest、preflight、golden corpus | 1 周 |
| I1 | Pipeline V2 数据模型与协议 | Rust schema、StageGraph V2、OpenAPI | 1 周 |
| I2 | Python 模型服务基础设施 | 可启动服务、registry、batch runtime | 1 周 |
| I3 | Layout 与公式检测闭环 | DocLayout-YOLO、YOLOv8-MFD | 1 周 |
| I4 | OCR 与公式识别闭环 | PaddleOCR、UniMERNet、公式回填 | 1–2 周 |
| I5 | 表格识别闭环 | SLANet-plus、cell matching、HTML | 1–2 周 |
| I6 | 文档组装与阅读顺序 | assemble、LayoutReader、para_split | 1 周 |
| I7 | uparser CLI 端到端集成 | 配置导入、服务调用、最终输出 | 1 周 |
| I8 | 性能、稳定性与可观测性 | batching、OOM、metrics、镜像 | 1 周 |
| I9 | 正式评测与发布门禁 | OmniDocBench、ODLB、评测报告 | 1 周 |

两人并行建议：ML 负责 I2–I6 的模型侧，CORE 负责 I1、I6–I8 的 Rust 侧，EVAL 工作从 I0 持续到 I9。关键路径仍由模型资产、oracle 和表格权重决定。

## 3. I0：资产、环境和基线冻结

### I0 Exit Gate

- 所有启用模型都有确定路径、hash、格式、来源和许可证标签。
- 能运行与 `magic-pdf.json` 匹配的参考 pipeline，或明确记录无法复原的部分。
- 30–50 页 golden corpus 和阶段级 oracle 已冻结。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T0.1 | 解析 `magic-pdf.json` 并定义模型逻辑名 | ML | 无 | 0.5d | 输出配置解析结果；模型名和路径单测通过 |
| T0.2 | 扫描模型目录并生成 SHA256 manifest | ML | T0.1 | 1d | `model-manifest.json`；重复执行结果稳定 |
| T0.3 | 核对模型格式、配置、词典和 tokenizer | ML | T0.2 | 1d | 每个模型列出必需文件；缺失项为结构化错误 |
| T0.4 | 补齐或确认 SLANet-plus 权重来源 | ML | T0.3 | 1–2d | 找到兼容权重及字典并记录 hash；否则标记 I5 阻塞 |
| T0.5 | 建立代码与模型许可证矩阵 | DEVOPS | T0.2 | 1d | `license-matrix.md`；区分 BYOM 和可分发 profile |
| T0.6 | 固定 Python/CUDA/PyTorch/Paddle 环境 | DEVOPS | T0.3 | 1–2d | lockfile 或镜像；GPU smoke test 通过 |
| T0.7 | 固定并运行当前 MinerU 参考 pipeline | ML | T0.6 | 2d | manifest 记录 version/commit；稳定产出新版 middle JSON 和 Markdown |
| T0.8 | 选择并冻结 golden corpus | EVAL | 无 | 1d | 覆盖扫描、原生、多栏、中英、公式、表格、旋转文本 |
| T0.9 | 导出双层 oracle | EVAL | T0.7,T0.8 | 2d | 新版 MinerU 端到端结果；legacy 权重阶段输出及 hash |
| T0.10 | 实现统一模型 preflight CLI | ML | T0.2,T0.3 | 1.5d | 对缺失文件、错误 hash、无 CUDA 给出可定位错误 |

## 4. I1：Pipeline V2 数据模型与协议

### I1 Exit Gate

- StageGraph 能表达整页 MFD、OCR spans、外部 OCR 表格和阅读顺序。
- Rust schema、Python schema 和 OpenAPI golden 完全一致。
- V1 现有 mock tests 不发生非预期回归。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T1.1 | 定义 V2 坐标系、Polygon、Region、ModelMetadata | CORE | I0 | 1d | serde round-trip、坐标合法性测试 |
| T1.2 | 定义 Layout/MFD batch request/response | CORE | T1.1 | 1d | schema golden 和异常输入测试 |
| T1.3 | 定义 OCR line/span batch schema | CORE | T1.1 | 1d | 保留 polygon、text、lang、confidence |
| T1.4 | 定义 MFR batch schema | CORE | T1.1 | 0.5d | `region_id` 可稳定回填 |
| T1.5 | 定义 Table input/output schema | CORE | T1.1,T1.3 | 1d | 输入 OCR cells，输出 HTML/cells/structure |
| T1.6 | 扩展 `StageKind` 和 `StageData` | CORE | T1.2–T1.5 | 1d | 新图能拓扑排序，错误依赖被拒绝 |
| T1.7 | 定义 V2 failure policy 和 warning schema | CORE | T1.6 | 0.5d | page-fatal 与 region-isolated 测试 |
| T1.8 | 生成或维护 OpenAPI 文档 | CORE | T1.2–T1.5 | 1d | OpenAPI 校验通过，示例可反序列化 |
| T1.9 | Python Pydantic schema 与 Rust contract 对齐 | ML | T1.8 | 1d | 双向 contract fixture 全部通过 |
| T1.10 | 制定 V1/V2 兼容和废弃策略 | CORE | T1.8 | 0.5d | 明确 endpoint、版本协商和错误码 |

## 5. I2：Python 模型服务基础设施

### I2 Exit Gate

- 服务可以在无模型模式完成 contract test，在 GPU 模式复用或兼容新版 MinerU runtime。
- 同一模型只加载一次，并能报告 revision、hash 和显存占用。
- batch runtime 能保序并隔离单项失败。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T2.1 | 创建 `services/pipeline-model-server` Python 包 | ML | I1 | 0.5d | 可安装、lint、unit test 通过 |
| T2.2 | 实现配置加载和 magic-pdf 兼容解析 | ML | T0.1,T1.9 | 1d | 兼容配置转换测试 |
| T2.3 | 实现 `/health`、`/v2/models` | ML | T2.1 | 0.5d | readiness 区分服务存活与模型可用 |
| T2.4 | 对齐新版 MinerU 线程安全 ModelSingleton/Registry | ML | T2.2 | 1.5d | 同配置并发只创建一个实例，不同 lang/feature 独立缓存 |
| T2.5 | 实现 CUDA device 和精度策略 | ML | T2.4 | 1d | cpu/cuda、fp32/fp16 配置验证 |
| T2.6 | 实现通用 batch executor | ML | T2.4 | 1.5d | 保序、超时、单项失败、取消测试 |
| T2.7 | 实现图像解码和输入约束 | ML | T1.9 | 1d | PNG/JPEG、尺寸上限、坏图测试 |
| T2.8 | 实现 request/model structured logging | DEVOPS | T2.3 | 1d | 日志含 request/page/model/latency，不输出图片内容 |
| T2.9 | 添加模型服务 contract/integration test harness | EVAL | T2.3,T2.6 | 1d | 无 GPU CI 可运行 stub 测试 |
| T2.10 | 提供开发启动命令和环境检查 | DEVOPS | T2.1–T2.9 | 0.5d | 一条命令启动并通过 health probe |

## 6. I3：Layout 与公式检测闭环

### I3 Exit Gate

- DocLayout-YOLO 和 YOLOv8-MFD 对 golden corpus 完成批量推理。
- 结果与 oracle 达到既定 box/category 一致性门禁。
- Rust 能并行调度两阶段并进行坐标归一化。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T3.1 | 封装 DocLayout-YOLO 模型加载 | ML | I2 | 1d | 权重 hash 校验、单页 smoke test |
| T3.2 | 复刻 layout 预处理和推理参数 | ML | T3.1,T0.9 | 1d | 固定 `1280/0.10/0.45`，结果可复现 |
| T3.3 | 合并 layout/MFD 并适配新版 MinerU 标准标签 | ML | T3.2,T3.6 | 1.5d | 输出与新版 `BatchAnalyze` 的 label/bbox/score 契约一致 |
| T3.4 | 实现 layout batch endpoint | ML | T3.3,T2.6 | 1d | 多页输出保序，坏页单独报错 |
| T3.5 | 封装 YOLOv8-MFD 模型加载 | ML | I2 | 1d | 权重 hash 校验、单页 smoke test |
| T3.6 | 复刻 MFD 预处理、标签和后处理 | ML | T3.5,T0.9 | 1.5d | inline/display 类别及框与 oracle 对齐 |
| T3.7 | 实现 MFD batch endpoint | ML | T3.6,T2.6 | 1d | batch、错误隔离、metadata 测试 |
| T3.8 | 实现 layout/MFD 坐标变换 | CORE | T1.1,T3.4,T3.7 | 1d | px/point/rotation round-trip 误差门禁 |
| T3.9 | 实现 layout 与 MFD 并行调度 | CORE | T1.6,T3.8 | 1d | mock 和真实服务集成测试 |
| T3.10 | 建立 layout/MFD 精度报告 | EVAL | T3.4,T3.7 | 1d | IoU、precision/recall、类别混淆矩阵 |

## 7. I4：OCR 与公式识别闭环

### I4 Exit Gate

- OCR 返回可定位的 line/span，不再只返回整段字符串。
- 公式区域在 OCR 前被正确遮罩，UniMERNet 结果按区域回填。
- 普通文本、行内公式和行间公式在 golden 页面上顺序正确。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T4.1 | 封装 PaddleOCR det 模型 | ML | I2,T0.3 | 1.5d | det 权重/字典校验和 smoke test |
| T4.2 | 实现公式区域 mask | ML | T3.7,T4.1 | 1d | 重叠公式不再生成普通 OCR 文本 |
| T4.3 | 复刻 OCR det 前后处理 | ML | T4.1,T0.9 | 2d | polygon 和 oracle 达到门禁 |
| T4.4 | 封装 PaddleOCR rec 模型及语言选择 | ML | T0.3 | 1.5d | ch/en 配置和错误语言测试 |
| T4.5 | 实现旋转裁剪与方向处理 | ML | T4.3,T4.4 | 1.5d | 旋转文本 fixture 通过 |
| T4.6 | 实现识别宽度分桶和 batch | ML | T4.4,T2.6 | 1.5d | 输出保序；padding 不改变文本 |
| T4.7 | 实现 OCR batch endpoint | ML | T4.2–T4.6 | 1d | line/span schema contract 通过 |
| T4.8 | 封装 UniMERNet-small 和 tokenizer | ML | I2,T0.3 | 2d | 权重、tokenizer、单公式 smoke test |
| T4.9 | 实现公式裁剪、分桶和 MFR batch endpoint | ML | T3.7,T4.8 | 2d | LaTeX 与 oracle 达到门禁 |
| T4.10 | 实现 inline/display 公式回填 | CORE | T4.7,T4.9 | 2d | 文本 span 与公式 span 无重复、顺序正确 |
| T4.11 | 建立 OCR/MFR 精度报告 | EVAL | T4.7,T4.9 | 1.5d | det、text edit、LaTeX 指标及错误样例 |

## 8. I5：表格识别闭环

### I5 Exit Gate

- 真实 SLANet-plus、PaddleOCR table matcher、有线/无线分类和方向校正已接入。
- 表格输出包含可渲染 HTML 和结构化 cells。
- 当前返回 tensor 长度的占位实现不再是默认路径。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T5.1 | 校验 SLANet-plus ONNX 输入输出和字典 | ML | T0.4 | 1d | shape、dtype、token dictionary 记录完整 |
| T5.2 | 复刻 SLANet-plus 图像预处理 | ML | T5.1 | 1.5d | 中间 tensor 与参考实现对齐 |
| T5.3 | 实现结构 token 解码 | ML | T5.1,T5.2 | 2d | token、bbox 与 oracle 对齐 |
| T5.4 | 移植 PaddleOCR table matcher | ML | T4.7,T5.3 | 2–3d | OCR 文本稳定匹配到 cells |
| T5.5 | 生成 canonical HTML 和 cell schema | ML | T5.4 | 1.5d | rowspan/colspan、转义和空 cell 测试 |
| T5.6 | 实现 table batch endpoint | ML | T5.5,T2.6 | 1d | 外部 OCR 输入、超时和单表失败测试 |
| T5.7 | Rust table stage 改为消费 OCR spans | CORE | T1.5,T5.6 | 1.5d | StageGraph external OCR 依赖验证通过 |
| T5.8 | 替换本地 ONNX 占位输出 | CORE | T5.3,T5.7 | 1d | 默认不再产生 tensor-length HTML 注释 |
| T5.9 | 增加有线/无线、旋转、表内公式/图片 fixtures | EVAL | T5.5 | 1d | fixture 覆盖新版 MinerU table pipeline 关键类型 |
| T5.10 | 建立 TEDS/cell/structure 报告 | EVAL | T5.6,T5.9 | 1.5d | 与 oracle 和 PaddleOCR 参考结果对比 |

## 9. I6：文档组装与阅读顺序

### I6 Exit Gate

- layout、OCR、公式、表格能合并为无重复的统一 blocks/spans。
- 对齐新版 MinerU post-OCR、公式编号、`para_split`、跨页表格和标题层级 finalize；LayoutReader 仅为可选兼容 backend。
- 多栏、列表、标题、页眉页脚和跨页段落测试通过。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T6.1 | 定义跨阶段 region merge 规则 | CORE | I3–I5 | 1d | 重叠、包含、优先级决策表 |
| T6.2 | 实现 block/span 去重和合并 | CORE | T6.1 | 2d | 公式/OCR/table 不重复输出 |
| T6.3 | 映射为 uparser Document IR | CORE | T6.2 | 1.5d | category/source/confidence/error 保真 |
| T6.4 | 封装 LayoutReader 推理 | ML | I2,T0.3 | 2d | 输入长度、bbox normalization 和输出测试 |
| T6.5 | 移植新版 MinerU document finalize 链路 | CORE | T6.3 | 2d | post-OCR、公式编号、para_split、跨页表格、标题层级 fixture 通过 |
| T6.6 | 实现 order backend 和 fallback | CORE | T6.4,T6.5 | 1d | 模型失败产生 warning 并稳定回退 |
| T6.7 | 实现页眉页脚和 discarded 策略 | CORE | T6.2 | 1d | 输出配置可控，无正文误删 |
| T6.8 | 实现跨页段落合并策略 | CORE | T6.3 | 1.5d | 连字符、列表和标题边界测试 |
| T6.9 | 建立阅读顺序和组装 golden tests | EVAL | T6.2–T6.8 | 1.5d | block sequence/edit distance 报告 |

## 10. I7：uparser CLI 端到端集成

### I7 Exit Gate

- 用户可以从 magic 配置生成 uparser profile、执行 preflight 并完成解析。
- 模型版本、错误和 warning 出现在结构化输出中。
- 无服务、坏配置、阶段超时均有明确且可操作的错误。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T7.1 | 定义 pipeline TOML/JSON 强类型配置 | CORE | I1,I6 | 1d | serde、默认值和未知字段测试 |
| T7.2 | 实现 magic-pdf 配置导入命令 | CORE | T2.2,T7.1 | 1d | 输入现有 JSON 可生成有效 profile |
| T7.3 | 实现 `uparser pipeline preflight` | CORE | T0.10,T7.1 | 1.5d | 服务、schema、模型、CUDA 检查汇总 |
| T7.4 | 将 Pipeline V2 adapter 接入 CLI | CORE | I3–I6,T7.1 | 2d | 单页和多页端到端命令通过 |
| T7.5 | 实现 MinerU processing-window 调度和 backpressure | CORE | T7.4 | 1.5d | 默认 64 页窗口、多文档切片、完成文档流式输出测试 |
| T7.6 | 完善模型 metadata、warning、error 输出 | CORE | T7.4 | 1d | JSON 中可追踪 stage/model/region |
| T7.7 | 验证 Markdown/JSON/asset 输出 | CORE | T6.3,T7.4 | 1.5d | 图片、公式、表格链接和内容正确 |
| T7.8 | 添加真实服务端到端测试集 | EVAL | T7.4–T7.7 | 2d | golden corpus 子集在 GPU 环境通过 |
| T7.9 | 更新用户和运维文档 | DEVOPS | T7.8 | 1d | 安装、配置、启动、排障和复现命令 |

## 11. I8：性能、稳定性与可观测性

### I8 Exit Gate

- 不存在按每个小区域单独同步请求导致的请求爆炸。
- OOM、超时、坏页和服务重启有确定行为。
- 性能数据可按 stage、model 和 batch size 定位。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T8.1 | 建立各阶段性能基线 | EVAL | I7 | 1d | 冷启动、P50/P95、吞吐、显存报告 |
| T8.2 | 将 JSON/base64 热点升级为 batch multipart/binary | ML,CORE | T8.1 | 2–3d | CPU/流量下降且 contract test 通过 |
| T8.3 | 调优 layout/MFD 页级 batch | ML | T8.1 | 1d | 给出设备 profile 的安全 batch |
| T8.4 | 调优 OCR/MFR 分桶和动态 batch | ML | T8.1 | 2d | 吞吐提升且精度 hash 不变 |
| T8.5 | 调优 table batch 和超时 | ML | T8.1 | 1d | 大表不阻塞其他页面 |
| T8.6 | 实现显存水位和 OOM 降 batch 重试 | ML | T8.3–T8.5 | 2d | 故障注入后任务可恢复或明确失败 |
| T8.7 | 实现长文档滑动窗口和流式落盘 | CORE | T7.5 | 2d | 长文档内存随窗口而非总页数增长 |
| T8.8 | 添加 Prometheus/结构化 metrics | DEVOPS | T2.8,T8.1 | 1.5d | 请求、阶段耗时、batch、错误、显存可查询 |
| T8.9 | 并发、超时、取消和服务重启测试 | EVAL | T8.6–T8.8 | 2d | 故障矩阵有自动测试和明确结论 |
| T8.10 | 构建 BYOM 服务镜像和启动检查 | DEVOPS | T8.9 | 1.5d | 镜像不内置受限权重，挂载模型后可运行 |

## 12. I9：正式评测与发布门禁

### I9 Exit Gate

- OmniDocBench 和 OpenDataLoader Bench 全量评测完成。
- 与旧 pipeline、uparser native/native+OCR、MinerU/VLM 的差异有明确结论。
- 准确率、性能、许可证和可复现性门禁全部通过。

| ID | Task | 角色 | 依赖 | 预估 | 产物与验收 |
|---|---|---|---|---:|---|
| T9.1 | 冻结候选版本代码、配置和模型 manifest | DEVOPS | I8 | 0.5d | commit/config/model hash 可追踪 |
| T9.2 | 运行 OmniDocBench pipeline 全量评测 | EVAL | T9.1 | 1–2d | text/formula/table/order/overall 完整结果 |
| T9.3 | 运行 OpenDataLoader Bench | EVAL | T9.1 | 1–2d | 官方指标和分布结果完整 |
| T9.4 | 与旧 pipeline oracle 比较 | EVAL | T9.2,T9.3 | 1d | 分类列出提升、回退和异常样例 |
| T9.5 | 与 uparser native/native+OCR 比较 | EVAL | T9.2,T9.3 | 1d | 给出不同文档类型的推荐路由 |
| T9.6 | 执行性能和稳定性最终门禁 | EVAL | T9.1 | 1d | 固定硬件上的可复现结果 |
| T9.7 | 完成许可证发布检查 | DEVOPS | T0.5,T8.10 | 1d | BYOM/redistributable 清单签核 |
| T9.8 | 更新架构评测报告 | EVAL | T9.2–T9.7 | 1.5d | 报告包含方法、环境、指标和结论 |
| T9.9 | 建立持续回归 job | EVAL | T9.8 | 1.5d | PR 小集、nightly 扩展集、release 全量集 |
| T9.10 | 发布评审和 go/no-go | 全体 | T9.8,T9.9 | 0.5d | 所有 gate 有证据，未通过项有 owner 和期限 |

## 13. 关键路径

```text
T0.1 -> T0.2 -> T0.3 -> T0.6 -> T0.7 -> T0.9
                                      |
                                      v
I1 -> I2 -> I3 -> I4 -> I6 -> I7 -> I8 -> I9
                  |
T0.4 ------------>I5 ----->I6
```

最高优先级阻塞项：

1. `T9.2`：已通过；latest 服务路径 Omni 1,651/1,651 页 Overall `88.4489`，0 evaluator fallback，
   高于官方 Pipeline `86.47`，相对同配置直接候选 `88.5123` 为 `-0.0634`。
2. `T9.3`：已通过；latest document API 在同一 ODL 200 PDF 上为 `0.857889`，高于 `0.856821`。
3. `T9.10`：Pipeline 目标与 VLM 全局上限必须分开声明；固定 Pipeline 模型尚未超过
   MinerU-VLM `0.923978/95.75`，若要求该门槛必须引入质量路由或 VLM fallback。
4. `I7/T9.9`：Rust runner 的 document API 生产接线及持续回归 job 尚未完成。
5. `T0.5/T9.7`：受限模型的发布方式和许可证签核未确认，会影响最终制品形态。

## 14. 并行执行建议

可以并行：

- T0.5 与 T0.6–T0.9。
- I1 的 Rust schema 与 I2 的服务骨架，但 Python contract 最终依赖 I1。
- I3 的 layout 与 MFD 两条模型任务。
- I4 的 OCR 与 MFR 模型加载任务。
- I5 的 SLANet 解码与 CORE 的 IR/table schema 准备。
- I8 的性能调优、metrics 和长文档处理。

不能提前并行：

- 未冻结 oracle 前进行 ONNX/TensorRT 转换。
- 未完成 OCR span schema 前实现 table external OCR 集成。
- 未完成跨阶段 merge 规则前优化最终 Markdown。
- 未冻结候选版本前执行正式全量评测。

## 15. Task Definition of Ready

一个 task 进入 `DOING` 前必须满足：

- 输入模型、接口或上游 task 已冻结。
- 验收 fixture、指标和阈值明确。
- 依赖和运行环境可获得。
- 已知许可证限制不会使任务产物无法使用。
- task 预计不超过 3 人日；超过则继续拆分。

## 16. Task Definition of Done

一个 task 只能在满足以下条件后标记 `DONE`：

- 代码或文档产物已提交到预定位置。
- 单元测试、contract test 或 golden test 已通过。
- 错误路径和日志信息已验证。
- 没有用占位结果冒充真实模型输出。
- 配置、模型 revision/hash 和复现命令已记录。
- 对准确率或性能有影响时，已更新对应基线结果。
- 后续 task 可以只依赖正式产物，不依赖开发者本地隐含状态。

## 17. 发布级完成标准

- `magic-pdf.json` 可导入为 uparser pipeline V2 profile。
- DocLayout-YOLO、YOLOv8-MFD、PaddleOCR、UniMERNet 和 SLANet-plus 均运行真实模型。
- LayoutReader 可选，`para_split` fallback 可独立工作。
- Pipeline V2 返回完整的 region/span/cell/metadata，而非单字符串或 tensor 占位。
- OmniDocBench 和 OpenDataLoader Bench 结果进入架构评测报告。
- 任一关键指标相对冻结参考基线下降超过 1 个百分点时阻断发布。
- 长文档、并发、OOM、超时和阶段失败均有测试覆盖。
- BYOM 和可分发 profile 的模型及许可证边界明确。
