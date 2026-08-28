# Pipeline V2 超越 MinerU 3.4.5 执行方案

## 0. 当前执行状态（2026-08-28）

结论：方案已经开始执行，但最终目标尚未完成，当前不能宣称精度超过 MinerU 3.4.5 Pipeline。

已完成：

- 建立 ODL/OmniDocBench Pareto 自动门禁及单元测试；当前历史结果仍为 FAIL，主要差距是 ODL TEDS 和 Omni Table/Text/Order。
- 修复 CLI partial failure 缺少 stderr 明细的问题，现会输出 page、stage 和 message。
- 对历史 9 个 OmniDocBench 失败页使用真实服务连续复测三轮，27/27 次成功；还需完成新实现的 1,651 页全量零失败验收。
- 实现 Rust/Python 对称的版本化二进制 tensor envelope，以及 Rust binary HTTP transport。
- 建立仅接受准备好 tensor、仅返回 raw tensor/token ID 的裸模型服务；不导入页面分析、Markdown、table binding 或 finalize 后端。
- 已验证真实/合成 forward：PP-DocLayoutV2、PP-OCRv6 detector、PP-OCRv6 recognizer、PP-FormulaNet-plus-M、table classifier、SLANet+、wired-table UNet。
- PP-DocLayoutV2 的 resize/normalize、raw top-k 解码、reading order、PaddleX filter、公式重标和页眉页脚修正已迁入 Rust。
- 真实 Omni 页面对照得到与 MinerU wrapper 相同的 9 个 region，类别、顺序和 bbox 一致。
- `pipeline_v2.rs` 已支持 `--bare-layout-endpoint`：一次裸 forward 在 Rust 同时生成 layout 和 MFD，不再需要这两个 Python structured endpoint。
- PP-OCRv6 detector 的 limit/normalize 输入、DB bitmap 连通域/旋转框/unclip 解码、四点旋转裁图，以及 recognizer 动态宽度输入和 CTC 词表解码已迁入 Rust；真实图与 MinerU 对拍 12/12 文本一致，框坐标差异约 1 px。
- PP-FormulaNet-plus-M 的 crop-margin、双阶段 resize、padding、normalize、HuggingFace tokenizer 解码和 LaTeX 修复已迁入 Rust；真实权重输出与 MinerU 3.4.5 逐字符串一致。
- SLANet+ 的 488 resize/pad、50 类 structure token、cell bbox、rowspan/colspan 解码已迁入 Rust；wired UNet 的 normalize、segmentation、morph close、线网格和 cell/span 恢复已迁入 Rust。
- 表格分类输入/logits、OCR span 按多边形交叠唯一绑定、Rust HTML 生成，以及 MinerU 3.4.5 wired/wireless 候选选择规则已迁入 Rust并有回归测试。
- 真实 wired-table 图上 Rust 与 MinerU 均恢复 13 个 cell 和完全相同的逻辑坐标；全裸 CLI 路径已输出正确的 13-cell HTML，`page_errors=[]`。
- `pipeline_v2.rs` 已支持裸 layout、OCR det/rec、formula、table classifier/SLANet/UNet；配置这些 endpoint 后，产品路径不再调用 Python structured stage。

仍未完成（按当前关键路径排序）：

1. 完成 OCR `merge_det_boxes`/`update_det_boxes` 的 Rust parity、表格方向分类、旋转 wired-table 修正及表内公式/图片绑定。
2. 将裸 endpoint profile 固化为默认生产配置，并增加启动时模型 revision/SHA 一致性校验；structured endpoint 只保留兼容模式。
3. 对开发集做精度差异定位，重点修正当前 ODL TEDS 差距，不以单页 parity 代替全量成绩。
4. 完成 ODL 200 文档和 OmniDocBench 1,651 页的精度、稳定性及同机五轮性能 A/B。

当前硬门禁状态：FAIL。只有本文第 2 节全部指标通过后，状态才能改为完成。

## 1. 目标与约束

目标是让 `pipeline_v2.rs` 的 Rust staged 路径在相同模型、硬件、并发和输入条件下，精度超过 MinerU 3.4.5 Pipeline。产品运行时除裸模型服务外，所有代码必须由 Rust 实现。

架构约束：

- Python 裸模型服务只允许加载模型/权重、接收已经准备好的模型输入张量、执行 `forward`/`generate`、返回原始 tensor 或 token ID。
- 图像解码、颜色空间转换、resize、padding、normalize、crop、batch 构造和坐标变换全部由 Rust 实现。
- 模型输出的 NMS、阈值过滤、类别映射、polygon/bbox 解码、OCR/公式 token 解码、表格结构解码全部由 Rust 实现。
- 文档级和页面级编排、区域归属、候选选择、表格内容绑定、Markdown/HTML 组装、阅读顺序、后处理、错误恢复全部由 Rust 实现。
- Python 服务不得读取 PDF、生成 Markdown/HTML、执行区域裁剪、绑定 OCR span、选择表格候选或调用 MinerU pipeline/finalize API。
- benchmark evaluator 使用 Python 不属于产品运行时依赖。
- `pages:analyze`、`documents:analyze` 和 MinerU official-finalize 只能作为对照基线，不能计入 `pipeline_v2.rs` 的成绩。

### 1.1 裸模型服务接口

每个服务进程只暴露单模型推理接口：

```text
ModelInfo -> model name/revision/input-output tensor schema/weight SHA
Infer     -> prepared tensors + generation parameters -> raw tensors/token IDs
Health    -> loaded/not-loaded/device/runtime
```

允许的 Python 依赖仅限模型框架、权重加载器和设备运行时。禁止在裸模型服务中导入本仓库的页面分析、表格装配、Markdown、reading-order 或 MinerU pipeline/finalize 模块。

为避免裸 tensor HTTP/JSON 造成性能和精度问题，传输使用带 dtype/shape/stride 描述的二进制协议；同机部署优先 Unix domain socket 或共享内存。JSON/base64 只允许用于调试，不作为正式 benchmark 路径。

## 2. 验收门槛

| 维度 | MinerU 基线 | Pipeline V2 目标 |
|---|---:|---:|
| ODL Overall | 0.856821 | >= 0.862 |
| ODL NID | 0.874540 | 不低于基线 |
| ODL TEDS | 0.910738 | >= 0.915 |
| ODL MHS | 0.791773 | 不低于基线 |
| ODL 性能 | 0.715631 s/篇 | <= 0.680 s/篇 |
| Omni Overall | 88.5123，同 PP-Formula 配置 | >= 88.75 |
| Omni Text Edit | 0.054230 | <= 0.0540 |
| Omni Formula CDM | 88.5788 | >= 88.70 |
| Omni Table TEDS | 82.3811 | >= 82.50 |
| Omni Order Edit | 0.147369 | <= 0.1470 |
| Omni 性能 | 重新同配置测定 | 至少快 5% |
| 稳定性 | 1,651/1,651 | 0 失败、0 非预期空白、0 evaluator fallback |

精度验收采用 Pareto 门禁：Overall 必须超过 MinerU 3.4.5 Pipeline，同时 NID、TEDS、MHS、Text、Formula、Table 和 Reading Order 任一主指标不得低于同模型 MinerU 基线。不能用某一项大幅提升掩盖另一项回退。

## 3. 阶段 0：冻结公平基线

预计时间：1 天。

1. 固定 MinerU 3.4.5 commit、模型 SHA、PP-Formula 配置、DPI、GPU、CUDA、并发数和 evaluator commit。
2. 在同一服务生命周期内依次运行 MinerU 与 Pipeline V2，避免冷启动和显存状态差异。
3. 性能分为冷启动、单 worker、最佳稳定吞吐三组；记录 p50、p95、GPU 利用率、显存和模型调用次数。
4. 建立 ODL 160/40 调优/held-out 划分；OmniDocBench 使用类别分层开发集，最终只跑一次全量验收。
5. 将基线写入机器可读 gate JSON，禁止继续引用不同 worker、不同公式模型的旧速度。

产物：`benchmark/results/pipeline_v2_gate_baseline_*.json`。

## 4. 阶段 1：解决 OmniDocBench 9 页失败

预计时间：1-2 天。

1. 对 9 个失败页逐阶段保存 layout、MFD、OCR、MFR、table 响应和 Rust warning。
2. 修复 CLI `exit 3` 时 stderr 为空的问题，输出失败 stage、page ID、错误码和 retryable 状态。
3. 区分像素上限、模型 OOM、HTTP 超时、响应契约错误和 Rust 装配错误。
4. 对 retryable 单页失败增加限流重试；OOM 使用像素预算或缩放降级，不能生成静默空白。
5. 为每类故障增加 fixture 回归测试。

门禁：OmniDocBench 1,651/1,651 成功，连续运行三次无随机失败。

## 5. 阶段 2：建立裸模型服务和 Rust 模型前后处理

预计时间：6-10 天。

Python 裸模型端点只允许：

- 加载模型和权重。
- 接收 Rust 已准备好的 tensor batch。
- 执行 `forward` 或生成模型的最小 `generate` 循环。
- 返回原始 tensor、token ID、shape、dtype 和模型 revision。

必须在 Rust 实现：

- PNG/JPEG 解码、颜色空间转换、normalize、resize、padding 和 batch tensor 构造。
- PP-DocLayoutV2 输出 tensor 解码、NMS、类别映射和坐标恢复。
- PP-OCRv6 检测/识别输入构造、输出解码和字符词表映射。
- PP-FormulaNet 输入构造、token ID 解码、停止条件和 LaTeX normalize。
- wired/wireless 表格模型输入构造、结构 token 解码、cell geometry 恢复。
- table region 裁剪策略和 OCR span 归属。
- 有线/无线模型结果选择。
- cell 与 OCR/formula 的唯一绑定。
- 表格 HTML/Markdown 组装。
- reading order、去重、block 修复、标题及段落处理。
- 空白页策略、错误恢复和跨 stage 编排。

为每个模型补充版本化 `TensorSchema` 和 golden tensor fixture。Rust 与 MinerU 原实现使用相同输入图片时，模型输入 tensor 必须逐元素一致或满足预先声明的浮点误差；Rust 解码结果必须与原模型 wrapper 的 bbox/token/cell 输出一致。

删除正式 profile 对 Python `page_analyzer.py`、`table_backend.py`、`mineru345_backend.py`、`mineru345_stage_backends.py` 业务逻辑的依赖。`pages:analyze`、`documents:analyze`、official-finalize 只能保留在 benchmark-reference profile，产品 profile 不得注册这些路由。

门禁：

1. 产品运行时 Python import trace 中不存在 MinerU pipeline/finalize 和本仓库业务后端模块。
2. 关闭所有 Python structured/finalize endpoint 后，Rust staged 仍可完整运行两套 benchmark。
3. 五类模型至少各有一组真实权重的 input tensor、raw output 和 Rust decode 对照测试。

## 6. 阶段 3：完整复刻 MinerU 3.4.5 表格链

预计时间：4-7 天。

这是当前最高优先级，因为最新 ODL TEDS 仍为 `0.863530`。

1. 接入表格方向分类，旋转 crop 后再识别，并把坐标反变换到原页。
2. 对表格同时运行 wired 与 wireless 路径。
3. 在 Rust 复刻 MinerU 的 cell 数量、文字命中数量和结构有效性选择规则。
4. 建立 OCR span 单一消费关系，解决重叠 table region 重复使用同一文本的问题。
5. 按 cell polygon 的交叠面积绑定文本，避免只按 bbox 中心判断。
6. 将公式和图片作为 inline object 注入对应 cell。
7. 对 rowspan、colspan、空单元格、旋转表格、嵌套表头建立 golden fixtures。
8. 保存每张表的两路候选、选择原因和最终 HTML，支持逐表诊断。

门禁：ODL held-out TEDS >= 0.915；OmniDocBench 开发集 Table TEDS 至少达到 MinerU 同配置水平。

## 7. 阶段 4：补齐 Rust 后处理

预计时间：3-5 天。

按独立消融依次实现，不能一次提交多个不可归因的启发式：

1. 高 IoU layout region 去重。
2. OCR span 去重和异常 span 清理。
3. block 包含、重叠和丢弃区域修复。
4. PP-DocLayoutV2 原生 reading order 与几何排序融合。
5. CJK 无空格拼接、英文断词重连、Markdown 转义。
6. 标题合并、段落拆分、跨栏文本边界。
7. 正确处理空白页，区分真实空白与模型失败。

每项必须记录 ODL/OmniDocBench 开发集 delta；任何主指标明显回退的规则不得合入。

门禁：ODL NID/MHS 和 OmniDocBench Text/Order 全部不低于 MinerU 基线。

## 8. 阶段 5：公式链对齐

预计时间：2-3 天。

1. 对 PP-FormulaNet 输入 crop、padding、resize、颜色空间逐像素对比 MinerU。
2. 对行内/块级公式分别验证类别、边界扩张和 Markdown delimiter。
3. 将公式去重、LaTeX normalize、非法输出修复全部留在 Rust。
4. 对 CDM 低分样本按裁剪错误、识别错误、装配错误分类。
5. 建立公式 hard-set，覆盖小公式、编号公式、低分辨率和表内公式。

门禁：OmniDocBench Formula CDM >= 88.70，且 Formula Edit 不回退。

## 9. 阶段 6：Rust 裸模调用性能优化

预计时间：3-5 天。

1. PP-DocLayoutV2 只执行一次 forward，Rust 从同一份 raw tensor 同时解码 layout 和公式区域，取消第二个 MFD 服务调用。
2. 将 OCR、MFR、table 按页面或区域真正 batch，避免逐区域请求。
3. 使用二进制 tensor 协议、Unix domain socket 或共享内存，禁止正式路径使用 PNG base64/JSON tensor。
4. Rust 侧使用有界并发，根据 GPU batch 容量施加背压。
5. 合并 table 专属 OCR 与页面 OCR 的公共 crop/preprocess 工作。
6. 复用 HTTP 连接，记录每阶段排队、网络、推理和装配耗时。
7. 使用固定顺序进行 batch 输出归并，不能以吞吐换确定性。

门禁：相同 worker 下 ODL <= 0.680 s/篇；OmniDocBench 比同配置 MinerU 快至少 5%，连续五轮变异系数低于 3%。

## 10. 阶段 7：最终全量验收

预计时间：2-3 天。

1. 运行 Rust 单元、契约、CLI、tensor golden 和 Python 裸模型 forward 测试。
2. 运行 ODL held-out，再运行完整 200 文档。
3. 运行 OmniDocBench 1,651 页完整生成。
4. 运行官方 page match、CDM、TEDS 全量评测。
5. 执行 MinerU 3.4.5 与 Pipeline V2 同机五轮性能 A/B。
6. 检查预测数量、空白页、fallback、timeout、错误和非确定性。
7. 输出逐指标差值、置信区间、模型 SHA、命令和原始结果哈希。
8. 对正式进程执行 Python import/call trace，证明 Python 只运行权重加载和模型 forward/generate。

只有全部 gate 通过，README 才能写“Pipeline V2 精度超过 MinerU 3.4.5 Pipeline”。服务内 official-finalize、`mineru-vlm`、Python structured endpoint 或不同模型取得的成绩不能归入 `pipeline_v2.rs`。

## 11. 工期与关键路径

预计总工期约 21-36 个工程日。严格裸模型边界会增加 Rust 模型预处理、tensor 解码和二进制传输协议的实现工作量。

关键路径：

1. 失败页闭环。
2. Python 边界收紧。
3. 表格完整复刻。
4. Rust 后处理。
5. 公式链对齐。
6. 性能批处理优化。
7. ODL 与 OmniDocBench 全量验收。
