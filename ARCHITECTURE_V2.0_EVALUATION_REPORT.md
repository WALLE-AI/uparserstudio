# uparser V2 架构评测报告

> 评测日期：2026-08-21；官方 evaluator 复核：2026-08-24；Pipeline 同模消融与 MinerU 3.4.5 复测：2026-08-25
> 当前 release 二进制 SHA-256：`557ae4b074380decb1c71ee6cf019b4a6a67f043745fa736ec98107a02016b44`  
> 构建：`cargo build --release -p uparser-core --features native,pdfium`  
> 所有 V2 性能运行均使用 `--no-cache`。

## 1. 结论

1. **V2 native 主链质量无回退**：200 文档的 Markdown 与冻结目录逐文件完全一致，Overall、NID、TEDS、MHS
   也逐项一致。冷运行 CLI 开销由 `47.34 ms/doc` 增至 `50.81 ms/doc`，增加 `3.47 ms/doc`（`7.33%`）。
2. **canonical renderer 当前不能切默认**：Overall 从 `0.875425` 降至 `0.566868`，199/200 个 Markdown
   与 engine renderer 不同，TEDS/MHS 均为 `0`。继续保留 engine renderer 是必要的 G-N 保护，而非未完成迁移。
3. **MinerU 经 V2 runner 的真实质量闸门通过，但综合精度没有超过旧版**：Overall `0.923978`，
   相对冻结 `0.928368` 下降 `0.004390`，小于允许的 `0.02`；200/200 成功。TEDS 提升
   `0.024334`，但 NID/MHS 分别下降 `0.003700`/`0.010514`。当前服务是
   `MinerU2.5-Pro-2605-1.2B`，冻结结果来自更早模型配置，因此这是发布闸门比较，不是纯框架微基准。
4. **auto 模式有明确的质量/成本收益曲线**：在 200 PDF 上，计划选择 native 156、MinerU 44；最终
   Overall `0.892023`，比纯 native 高 `0.016598`，TEDS 高 `0.091942`。总耗时 `27.40s`，是纯 native
   的 `2.70x`，但比全量 MinerU 快 `4.53x`。
5. **格式和 preflight 性能达到本地契约**：权威 16 变体（15 个可识别格式 + Unknown）及额外 XLSX
   物理变体共 17 个真实/受控 fixture，内容检测与固定策略路由一致性均为 `100%`。200 PDF plan
   零失败，吞吐 `20.32 docs/s`，延迟中位数 `48.36ms`、P95 `60.39ms`。
6. **完整 OmniDocBench 已通过当前 V2 模型链实跑，但质量未超过历史最佳**：1,651/1,651 页生成完成，
   非零返回码为 0。2026-08-25 用当前 evaluator 全量复核后，文本/公式 Edit distance 为
   `0.069960`/`0.102585`，表格 TEDS/结构 TEDS 为 `0.906068`/`0.937463`，阅读顺序 Edit distance
   为 `0.136131`，公式 CDM leaderboard 值为 `88.8433`。page match 有 1 页使用 timeout fallback；
   2,352 个公式 CDM 和 665 个表格 TEDS 均无超时、错误或异常。
   有 1 页成功返回但 Markdown 仅含换行，作为已知空输出保留。
7. **严格同模 A/B 证明 Pipeline V2 存在重构精度损失**：相对加载完全相同旧权重的原始 MinerU
   1.3.5 Pipeline，OpenDataLoader Overall 从 `0.852981` 降到 `0.800122`（`-0.052859`），
   OmniDocBench Overall 从 `76.1250` 降到 `75.7789`（`-0.3460` 分），空白页从 2 增至 60。
   因此不能再把差距仅归因于旧模型资产。
8. **uparser MinerU-VLM 的官方对比呈现两种结论**：OpenDataLoader-Bench Overall `0.923978`，比公开的
   MinerU 2.7 pipeline 高 `0.092842`；OmniDocBench 当前官方口径 Overall `91.4251`，比相同模型族
   `MinerU2.5-Pro` 官方结果 `95.75` 低 `4.3249` 分，最大差距是公式 CDM `-8.6067` 分。因此可以
   声明 OpenDataLoader 输出精度领先该公开 pipeline 结果，不能声明 VLM 综合精度超过官方模型结果。
9. **MinerU 3.4.5 新模型 Pipeline 大幅缩小差距但仍未完全对齐官方参考**：ODL Overall
   `0.856821`；Omni Overall `85.3391`，比 uparser Pipeline V2 高 `9.5602` 分，比公开官方
   Pipeline `86.47` 低 `1.1309` 分。表格已略高于参考，主要差距为公式 CDM `-3.4874` 分。

## 2. Bench A 对比

数据集为 `opensource/opendataloader-bench/pdfs` 的 200 个 PDF；分数越高越好。

| 路径 | Overall | NID | TEDS | MHS | 秒/文档 | 执行失败 |
|---|---:|---:|---:|---:|---:|---:|
| 冻结 native | 0.875425 | 0.915019 | 0.814117 | 0.787511 | 0.047341 | 1 个 explicit-native 拒绝/空输出 |
| **V2 native + engine** | **0.875425** | **0.915019** | **0.814117** | **0.787511** | **0.050810** | 1 个相同拒绝/空输出 |
| V2 native + canonical | 0.566868 | 0.852652 | 0.000000 | 0.000000 | 0.051724 | 1 个相同拒绝/空输出 |
| 冻结 MinerU | 0.928368 | 0.947010 | 0.943894 | 0.877728 | 不采用（旧运行命中缓存） | 未记录 |
| **V2 MinerU 2605** | **0.923978** | **0.943310** | **0.968228** | **0.867214** | **0.620810** | **0/200** |
| **V2 auto** | **0.892023** | **0.928040** | **0.906059** | **0.794805** | **0.136988** | **0/200** |
| 原始 MinerU 1.3.5 Pipeline（同模） | 0.852981 | 0.880607 | 0.856388 | 0.786227 | 1.226984 | 0/200 |
| MinerU 3.4.5 Pipeline（PP-DocLayoutV2） | 0.856821 | 0.874540 | 0.910738 | 0.791773 | 0.715631 | 0/200 |
| Pipeline V2（旧权重） | 0.800122 | 0.832794 | 0.836368 | 0.700005 | 0.970612 | 0/200 |
| 官方 MinerU 2.7.0 | 0.831135 | 0.857362 | 0.872992 | 0.742983 | 5.961504（Apple M4） | 0/200 |

MinerU 与 auto 使用 4 个文档级 worker；每个文档内部 runner 的 `max-concurrency=16`。服务已预热，
但 uparser 内容缓存关闭。V2 MinerU 相对冻结结果：Overall `-0.004390`、NID `-0.003700`、
TEDS `+0.024334`、MHS `-0.010514`，各质量项均未触发 `-0.02` 回退线。

因此 Bench A 的结论是**表格能力提升，但综合精度没有提升**。V2 的 Overall、阅读顺序和标题均略低于
冻结版本；“通过质量闸门”只表示回退在发布容差内，不等于优于旧版。

auto 相对纯 native：Overall `+0.016598`、NID `+0.013021`、TEDS `+0.091942`、MHS `+0.007293`。
这证明路由在该语料上产生净收益，但不替代 G-R 的人工标签和 best-feasible-mode regret 评测。

Pipeline V2 相对官方 MinerU 2.7.0：Overall `-0.031014`、NID `-0.024568`、TEDS `-0.036623`、
MHS `-0.042978`；结构简化指标 TEDS-S/MHS-S 分别 `+0.012694`/`+0.020582`。本次运行在 A100
服务上，官方速度来自 Apple M4，不能用墙钟直接判断速度优劣。

严格同模下，Pipeline V2 相对原始 MinerU 的 Overall/NID/TEDS/MHS 分别为 `-0.052859`/
`-0.047814`/`-0.020020`/`-0.086222`。MinerU 3.4.5 相对旧同模原始实现 Overall 为
`+0.003841`，其中 TEDS `+0.054351`，说明新版主要收益来自表格链；但它同时更换了模型与实现，
不能作为纯框架消融。

uparser MinerU-VLM V2 相对官方 MinerU 2.7.0：Overall `+0.092842`、NID `+0.085948`、
NID-S `+0.091557`、TEDS `+0.095237`、TEDS-S `+0.070261`、MHS `+0.124231`、MHS-S
`+0.070031`。但本地路径是 `MinerU2.5-Pro-2605-1.2B` VLM + uparser + A100 服务，官方路径是
MinerU 2.7 pipeline + Apple M4；这里比较的是同一数据集上的产物精度，不是同模型、同框架或同硬件消融。

## 3. OmniDocBench 对比

完整 `OmniDocBench.json` 共 1,651 页。当前结果使用 release 二进制、`mineru-vlm` 协议、
`MinerU2.5-Pro-2605-1.2B`、4 个生成 worker，并固定 `--no-cache`。官方评测使用 quick-match 24 workers；
Edit distance 越低越好，TEDS 越高越好。

| 路径 | 文本 Edit | 公式 Edit | 表格 TEDS | 结构 TEDS | 阅读顺序 Edit |
|---|---:|---:|---:|---:|---:|
| **当前 V2（2026-08-25 evaluator/CDM 复核）** | **0.069960** | **0.102585** | **0.906068** | **0.937463** | **0.136131** |
| 历史 `mineru-vlm-v2`（2026-08-13） | 0.070586 | 0.094509 | 0.901686 | 0.930939 | 0.137773 |
| 历史 `mineru-vlm-2605-current` | 0.068201 | 0.099888 | 0.913716 | 0.944593 | 0.138681 |
| 历史 `mineru-vlm-2605-official` | 0.037728 | 0.093537 | 0.919973 | 0.948615 | 0.129622 |
| 历史 `mineru-vlm-2605-surpass-e1-full` | 0.036731 | 0.094840 | 0.906502 | 0.938830 | 0.128454 |

相对历史 `mineru-vlm-v2`，当前 V2 文本 Edit 改善 `0.000626`、表格 TEDS 提升 `0.004382`、结构
TEDS 提升 `0.006524`、阅读顺序 Edit 改善 `0.001642`，但公式 Edit 退化 `0.008076`。相对更接近的
`mineru-vlm-2605-current`，只有阅读顺序 Edit 改善 `0.002550`；文本/公式 Edit 分别退化
`0.001759`/`0.002697`，表格 TEDS/结构 TEDS 分别下降 `0.007648`/`0.007130`。

相对历史最佳 `mineru-vlm-2605-surpass-e1-full`，文本/公式/阅读顺序 Edit 分别退化
`0.033229`/`0.007745`/`0.007677`，表格 TEDS 基本持平（`-0.000434`）。因此 OmniDocBench 的结论也是
**运行完整性和稳定性提升或通过，但综合精度没有提升**，不能宣称质量超过历史最佳或 official 基线。

生成输出为 1,651 个 Markdown，非零返回码 0、空文件 0；其中 1 个文件仅含换行。2026-08-25 复核中，
官方 page match 的 1,651 页有 1 页 timeout fallback；CDM 2,352 个样本和 TEDS 665 个样本均无
timeout/error/exception。
生成文件首末写入跨度 `2,024.45s`（约 `1.23 pages/s`）；本次官方页匹配约 `1,397s`，公式 CDM
约 414s，表格 TEDS 约 96s。该墙钟数据包含
服务排队与不同页面复杂度，仅作为本机实测，不替代独立端到端计时基准。

### 3.1 uparser MinerU-VLM 与官方 MinerU2.5-Pro

为与当前 OmniDocBench 榜单对齐，2026-08-25 使用官方 v1.7 evaluator commit
`193627ae9e97d89188468ed1ee3b7a856ff76044`，在原 1,651 页预测上补跑 CDM。Edit 越低越好，
其他指标越高越好。

| 路径 | Overall | 文本 Edit | 公式 CDM | 表格 TEDS | 结构 TEDS | 阅读顺序 Edit |
|---|---:|---:|---:|---:|---:|---:|
| **uparser MinerU-VLM V2** | **91.4251** | **0.069960** | **88.8433** | **92.4279** | **95.2449** | **0.136131** |
| 官方 MinerU2.5-Pro | 95.75 | 0.036 | 97.45 | 93.42 | 95.92 | 0.120 |
| 差值 | -4.3249 | +0.033960 | -8.6067 | -0.9921 | -0.6751 | +0.016131 |

公式 CDM 是主要差距，表格 TEDS/结构 TEDS 已较接近官方值。这里的 TEDS 是官方 leaderboard
按 458 个含表格页面计算的 page average；上方历史表中的 `0.906068` 是 evaluator 另行输出的 665
个表格 sample aggregate，两者不能混用。页面匹配有 1 页 timeout fallback；2,352 个 CDM 样本和
665 个 TEDS 样本均为 0 timeout、0 error、0 exception。

CDM 算法来自官方 v1.7，但本机使用 TeX Live 2022 + `pdftocairo` 兼容路径，并出现 Euler 位图字体
缺失警告；公开榜单的数据集标记为 `v1.6_full`，本地 1,651 页数据则使用记录 commit 的 v1.7 代码
评测，不应宣称为官方打包环境的严格复现。即使按当前有效输出，Overall 仍低 `4.3249` 分，
因此发布判断是**完整性通过，官方 VLM 精度对齐不通过**。

### 3.2 Pipeline V2 与官方 MinerU-Pipeline

本次使用当前 OmniDocBench v1.7、1,651 页、quick-match 24 workers、CDM 8 workers 和 TEDS 24
workers。Edit 越低越好，其他指标越高越好。

| 路径 | Overall | 文本 Edit | 公式 CDM | 表格 TEDS | 结构 TEDS | 阅读顺序 Edit |
|---|---:|---:|---:|---:|---:|---:|
| **Pipeline V2** | **75.7789** | **0.190804** | **73.3099** | **73.1073** | **84.7475** | **0.294770** |
| 官方 MinerU-Pipeline | 86.47 | 0.055 | 83.07 | 81.88 | 88.68 | 0.153 |
| 差值 | -10.6911 | +0.135804 | -9.7601 | -8.7727 | -3.9325 | +0.141770 |

Pipeline 预测 1,651/1,651 成功，但 60 页仅包含空白 Markdown；page match 有 2 页使用 timeout
fallback。2,352 个公式 CDM 和 665 个表格 TEDS 均为 0 timeout、0 error、0 exception。CDM 使用
官方 v1.7 算法，但本机公式栅格化为 TeX Live 2022 + `pdftocairo` 兼容路径，并出现 Euler 位图字体
缺失警告，不应将该 CDM 数值宣称为官方 Docker 环境的严格复现。

### 3.3 Pipeline 严格同模消融

原始 `magic-pdf 1.3.5` 与 uparser Pipeline V2 对同一 1,651 页加载完全相同的 DocLayout-YOLO、
YOLO MFD、UniMERNet-small、PaddleOCR、SLANet-plus 和 LayoutReader 权重。

| 路径 | Overall | 文本 Edit | 公式 CDM | 表格 TEDS | 结构 TEDS | 阅读顺序 Edit | 空白页 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 原始 MinerU 同模 Pipeline | 76.1250 | 0.153716 | 68.6217 | 75.1248 | 82.5540 | 0.266186 | 2 |
| uparser Pipeline V2 | 75.7789 | 0.190804 | 73.3099 | 73.1073 | 84.7475 | 0.294770 | 60 |
| uparser - 原始 | -0.3460 | +0.037088 | +4.6882 | -2.0175 | +2.1935 | +0.028584 | +58 |

公式 CDM 与表格结构分数提升，但文本、顺序、表格内容与完整性退化；ODL 的标题 MHS 另有
`-0.086222` 的显著回退。结论是模型调用本身并非全面变差，主要损失位于模型输出后的结构清洗、
阅读顺序、段落/标题恢复和表格内容装配。

### 3.4 MinerU 3.4.5 新模型 Pipeline

本地 `opensource/MinerU` 版本 `3.4.5`、commit `4fe4bde1`，使用 PP-DocLayoutV2、表格方向分类、
有线 UnetStructure、无线 SLANet-plus 和当前 OCR/公式模型。它属于“新模型 + 新实现”系统比较，
不是同模框架消融。

| 路径 | Overall | 文本 Edit | 公式 CDM | 表格 TEDS | 结构 TEDS | 阅读顺序 Edit | 空白页 |
|---|---:|---:|---:|---:|---:|---:|---:|
| MinerU 3.4.5 Pipeline | 85.3391 | 0.056176 | 79.5826 | 82.0523 | 88.8441 | 0.153534 | 1 |
| 官方 MinerU-Pipeline 参考 | 86.47 | 0.055 | 83.07 | 81.88 | 88.68 | 0.153 | 未公布 |
| 3.4.5 - 官方参考 | -1.1309 | +0.001176 | -3.4874 | +0.1723 | +0.1641 | +0.000534 | - |

生成 1,651/1,651 成功，墙钟 `1,656.24s`（`1.00317s/page`）；page match 无 fallback，2,352
个 CDM 和 665 个 TEDS 样本均为 0 timeout/error/exception。公式是当前主要剩余差距。

## 4. 格式、分析与路由

fixture 覆盖 PDF、DOC、DOCX、PPT、PPTX、Excel（XLS 与 XLSX）、ODT、ODS、ODP、RTF、EPUB、
CSV、TSV、PNG、JPEG 和 Unknown。每个格式重复 5 次：

| 指标 | 结果 |
|---|---:|
| 格式契约一致性 | 17/17（100%） |
| 固定 V2 策略路由一致性 | 17/17（100%） |
| Unknown 拒绝 | 5/5，稳定 exit code 1 |
| 200 PDF plan 失败 | 0 |
| 200 PDF plan 吞吐 | 20.32 docs/s |
| plan 延迟 mean / median / P95 / max | 49.21 / 48.36 / 60.39 / 92.06 ms |
| 路由分布 | native 156；mineru-vlm 44 |

类型分布为 general report 108、resume 50、unknown 16、academic paper 13、regulation 7、contract 4、
legal document 2。这里的 `100%` 是检测与已冻结策略的契约一致性，不是语义分类准确率；没有人工标签时
不得将它写成 G-R 通过。

## 5. 外部模式可用性

| 模式 | 当前证据 | 状态 |
|---|---|---|
| native | 200 文档质量、逐文件 diff、冷运行性能 | G-N engine 路径通过 |
| mineru-vlm | 真实 2605 服务，200 文档无缓存运行 | 当前服务 G-A/G-B 质量容差通过 |
| auto | 200 文档端到端执行，156/44 路由分布 | 本语料 route-result 有收益；G-R 标签闸门待跑 |
| generic-vlm | 无匹配的通用 VLM 输出契约服务 | 未评测 |
| paddlex-structure | `doctor`：`localhost:8080/layout-parsing` unreachable | 未评测 |
| pipeline | V2 服务和六阶段真实后端已实现；OpenDataLoader 200/200、OmniDoc 1,651/1,651 完成 | 链路通过；精度低于官方 MinerU pipeline |

### 5.1 Pipeline V2 单页实现证据（2026-08-25）

架构与端到端行为当前参考本地 MinerU 3.4.5、commit
`4fe4bde114a23ee5dd637eae99b767f4669bf58c`；旧 `magic_pdf` 用于复用配置指定的模型权重，并作为
严格同模原始实现复跑。

| 证据 | 结果 |
|---|---:|
| Layout CPU smoke | 15 regions / 5.266s |
| MFD CPU smoke | 1 region / 3.642s |
| OCR CPU smoke | 25 spans / 102.188s |
| MFR CPU smoke | 1 formula / 2.224s |
| 表格整页 CPU smoke | 20 regions、40 OCR spans、6 formulas、1 table/60 cells / 95.645s |
| Python 服务测试 | 22/22 通过 |
| Rust V2 目标测试 | 5/5 通过 |
| OpenDataLoader-Bench | 200/200；Overall 0.800122；0.970612s/doc |
| OmniDocBench | 1,651/1,651；Overall 75.7789；生成 2.342470s/page |
| 原始 MinerU 同模 ODL/Omni | 200/200 Overall 0.852981；1,651/1,651 Overall 76.1250 |
| MinerU 3.4.5 ODL/Omni | ODL 200/200 Overall 0.856821；Omni 1,651/1,651 Overall 85.3391 |

表格页第一次运行只得到空 cell，原因是正文 OCR 按 MinerU 语义跳过 table region；补齐独立 table OCR
支路并将带 bbox 的公式 span 送入 matcher 后，HTML 从空表变为包含真实表头和单元格文本的 759 字符
表格。uparser 当前 `model-manifest` 能力明确为 `wireless-only`：它引用的旧资产没有新版 MinerU
的有线/无线分类、表格方向和 UnetStructure 权重，不得将该 smoke 写成完整新版表格 pipeline
对齐；另行测试的 MinerU 3.4.5 已加载这些新资产。

同模诊断表明当前短板不是权重缺失这一项：`benchmark/run_pipeline_v2_benchmarks.py` 的 renderer
直接把 OCR span 按换行拼接；`compat.py` 没有原始 `MagicModel` 的低分/高 IoU 清理；原始 MinerU
还执行 span 去重、block 修复、标题合并、LayoutReader 排序、`para_split` 和语言感知 Markdown
构建。表格 backend 则按 bbox 中心为每个 table region 独立选择 span，重叠 region 会重复消费内容。
这些缺口分别对应同模 NID/MHS 回退、60 个空白页和重复表格输出。

原始证据：`pipeline/model-manifest.json`、`pipeline/openapi-v2.json`、
`pipeline/oracle/detection-smoke-cpu.json`、`pipeline/oracle/recognition-smoke-ocr-cpu.json`、
`pipeline/oracle/recognition-smoke-mfr-cpu.json`、`pipeline/oracle/page-pipeline-table-cpu.json`。

## 6. 原始产物

OpenDataLoader-Bench 结果：

- `opensource/opendataloader-bench/prediction/uparser-native-v2-engine/evaluation.json`
  SHA-256 `6f6b384cd24c2ee22604f18581cf230ec1b3f241091c452fe07bbb65e376681b`
- `opensource/opendataloader-bench/prediction/uparser-native-v2-canonical/evaluation.json`
  SHA-256 `bac90c68791c80b01d95856dd1aa575e13b85a95fc079f6967bce42c9fdd2d90`
- `opensource/opendataloader-bench/prediction/uparser-mineru-vlm-v2/evaluation.json`
  SHA-256 `b9905a08b1cff0c040dd68d562036d51ce1c7fab3f7953778d134bc6703ac56f`
- `opensource/opendataloader-bench/prediction/mineru/evaluation.json`（官方 MinerU 2.7.0）
  SHA-256 `b164486126889ee333e28d1f7a611891fe40bc944b9efc4fa7a7efeb3dbd2c27`
- `opensource/opendataloader-bench/prediction/uparser-auto-v2/evaluation.json`
  SHA-256 `fab547629964fcbb5246b88e16a5b2dd2c486d4fb8ac5d1ce0bac20f48442d38`
- `opensource/opendataloader-bench/prediction/uparser-pipeline-v2-20260825/evaluation.json`
  SHA-256 `abed4e0783c7b47551b6ebfc437ae47592ae5022019db366e089d38919ff1673`
- `opensource/opendataloader-bench/prediction/mineru-original-pipeline-same-models-20260825/evaluation.json`
  SHA-256 `ca9d5babb98d6af9582b04c8a8a53f5833ee18e548bc9a1edc288ff745f381a9`
- `opensource/opendataloader-bench/prediction/mineru-3.4.5-pipeline-20260825/evaluation.json`
  SHA-256 `f9697c6247c7978b571263bb5ca518147a202dbf5c9379ddf611320e0907a406`
- `benchmark/results/architecture_v2_20260821.json`：格式、策略和 preflight 延迟明细。
- `benchmark/results/architecture_v2_bench_a_20260821.json`：可提交的 Bench A 精确指标、运行参数、差值和原始产物哈希。

OmniDocBench 结果：

- `benchmark/results/architecture_v2_omnidoc_metric_20260821.json`（官方 metric result 原样副本）
  SHA-256 `dd299fa7d498fb6ddf26650b900afa5d00ffb1078f50a42fef7551fb4b35eccb`
- `benchmark/results/architecture_v2_omnidoc_summary_20260821.json`（官方 run summary 原样副本）
  SHA-256 `c44a39544e0934d178a93c4b72c881b2fc058e3f01904f170ec8b23ad39869d5`
- `benchmark/OmniDocBench/result/architecture-v2-20260821_quick_match_*.json`：嵌套评测仓库中的原始输出。
- `benchmark/OmniDocBench/result/architecture-v2-20260821_quick_match_metric_result.json`
  SHA-256 `028e4a3edffeaa40844a261c5cb28b34383f8810d32bf19b458a0647fe1d728e`
- `benchmark/OmniDocBench/result/architecture-v2-20260821_quick_match_run_summary.json`
  SHA-256 `60eb815075ae957edcb5e595f23a387fa18917d54380f44355f5522e1eee50c2`
- `benchmark/omnidoc_pred/architecture-v2-20260821/`：1,651 个当前 V2 Markdown 预测。
- `benchmark/OmniDocBench/result/pipeline-v2-20260825_quick_match_metric_result.json`
  SHA-256 `5e5f489ba668baffe77e6f0a8e78ca3279b20aec361ecd5156debf43aa98335b`
- `benchmark/OmniDocBench/result/pipeline-v2-20260825_quick_match_run_summary.json`
  SHA-256 `33b3a16edb33c749d0ff20d9d15f6c3a4e37c0a4b887cc9cc9a45bf263ec7aa8`
- `benchmark/OmniDocBench/result/mineru-original-pipeline-same-models-20260825_quick_match_metric_result.json`
  SHA-256 `32f06ff681e8a9969ca9ef4d8a3b91bd4a483b293f6044e72c5f544c74329190`
- `benchmark/OmniDocBench/result/mineru-3.4.5-pipeline-20260825_quick_match_metric_result.json`
  SHA-256 `58652da106a8691657fb8ca9dd57e3a0cd7313ab86c9f0ceb9f9dbe9a29451fc`
- `benchmark/OmniDocBench/result/mineru-3.4.5-pipeline-20260825_quick_match_run_summary.json`
  SHA-256 `2a135b8f523e7769d48117ec7b7d7080ea9bcb4261a1712aa8709b424003e1a6`
- `benchmark/results/pipeline_v2_accuracy_20260825.json` 和 `.md`：Pipeline 两套全量基准、官方差值及环境限制。
- `benchmark/results/pipeline_comparison_20260825.json` 和 `.md`：原始同模、uparser V2、MinerU 3.4.5
  三方实测、差值、输出诊断和归因结论。
- `benchmark/results/mineru_vlm_accuracy_20260825.json` 和 `.md`：MinerU-VLM 两套全量基准、官方差值、
  evaluator 健康状态及环境限制。

上述目录受仓库既有 `.gitignore` 管理；本报告固化关键数字和哈希，原始产物保留在当前工作区供复核。

## 7. 发布判断

- V2 runner/native 保真：**通过**。
- 默认 renderer 切换：**不通过，继续使用 engine**。
- 当前 MinerU model-protocol：**质量容差通过**；旧速度记录作废，新性能以 `0.620810s/doc` 为准。
- 当前 MinerU OmniDocBench：**完整性和稳定性通过，综合精度未提升**；相对历史 `mineru-vlm-v2`
  有增有退；当前官方口径 Overall `91.4251`，比 MinerU2.5-Pro 官方 `95.75` 低 `4.3249` 分，
  不能标记为质量领先。OpenDataLoader 精度虽全面高于公开 MinerU 2.7 pipeline，因路线不同不能据此
  归因于 uparser 框架提升。
- auto：**本语料端到端收益通过**；未达到“人工标注 G-R 已通过”的证据级别。
- Pipeline V2：**链路完整性通过，精度门禁不通过**；严格同模 ODL/Omni Overall 分别回退
  `0.052859`/`0.3460` 分，必须补齐 MinerU 后处理与内容装配，不能只更换模型。
- MinerU 3.4.5 Pipeline：**本地新模型链评测通过，官方参考对齐仍差 `1.1309` 分**；表格已对齐，
  后续重点是公式 CDM，而不是继续调整表格模型。
