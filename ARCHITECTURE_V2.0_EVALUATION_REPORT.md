# uparser V2 架构评测报告

> 评测日期：2026-08-21  
> 当前 release 二进制 SHA-256：`557ae4b074380decb1c71ee6cf019b4a6a67f043745fa736ec98107a02016b44`  
> 构建：`cargo build --release -p uparser-core --features native,pdfium`  
> 所有 V2 性能运行均使用 `--no-cache`。

## 1. 结论

1. **V2 native 主链质量无回退**：200 文档的 Markdown 与冻结目录逐文件完全一致，Overall、NID、TEDS、MHS
   也逐项一致。冷运行 CLI 开销由 `47.34 ms/doc` 增至 `50.81 ms/doc`，增加 `3.47 ms/doc`（`7.33%`）。
2. **canonical renderer 当前不能切默认**：Overall 从 `0.875425` 降至 `0.566868`，199/200 个 Markdown
   与 engine renderer 不同，TEDS/MHS 均为 `0`。继续保留 engine renderer 是必要的 G-N 保护，而非未完成迁移。
3. **MinerU 经 V2 runner 的真实质量闸门通过**：Overall `0.923978`，相对冻结 `0.928368` 下降
   `0.004390`，小于允许的 `0.02`；200/200 成功。当前服务是
   `MinerU2.5-Pro-2605-1.2B`，冻结结果来自更早模型配置，因此这是发布闸门比较，不是纯框架微基准。
4. **auto 模式有明确的质量/成本收益曲线**：在 200 PDF 上，计划选择 native 156、MinerU 44；最终
   Overall `0.892023`，比纯 native 高 `0.016598`，TEDS 高 `0.091942`。总耗时 `27.40s`，是纯 native
   的 `2.70x`，但比全量 MinerU 快 `4.53x`。
5. **格式和 preflight 性能达到本地契约**：权威 16 变体（15 个可识别格式 + Unknown）及额外 XLSX
   物理变体共 17 个真实/受控 fixture，内容检测与固定策略路由一致性均为 `100%`。200 PDF plan
   零失败，吞吐 `20.32 docs/s`，延迟中位数 `48.36ms`、P95 `60.39ms`。
6. **完整 OmniDocBench 已通过当前 V2 模型链实跑**：1,651/1,651 页生成完成，非零返回码为 0；
   官方 quick-match 与 665 个表格 TEDS 均无超时、错误或异常。文本/公式 Edit distance 为
   `0.069660`/`0.102585`，表格 TEDS/结构 TEDS 为 `0.906068`/`0.937463`，阅读顺序 Edit distance
   为 `0.135750`。有 1 页成功返回但 Markdown 仅含换行，作为已知空输出保留。
7. **尚不能宣称所有外部模式通过**：本机没有 PaddleX `:8080` 或 pipeline `:9001` 服务，故
   `paddlex-structure`、generic-vlm 专用服务、Mode 3 pipeline Bench A 以及人工标注 G-R regret 仍待验证。

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

MinerU 与 auto 使用 4 个文档级 worker；每个文档内部 runner 的 `max-concurrency=16`。服务已预热，
但 uparser 内容缓存关闭。V2 MinerU 相对冻结结果：Overall `-0.004390`、NID `-0.003700`、
TEDS `+0.024334`、MHS `-0.010514`，各质量项均未触发 `-0.02` 回退线。

auto 相对纯 native：Overall `+0.016598`、NID `+0.013021`、TEDS `+0.091942`、MHS `+0.007293`。
这证明路由在该语料上产生净收益，但不替代 G-R 的人工标签和 best-feasible-mode regret 评测。

## 3. OmniDocBench 对比

完整 `OmniDocBench.json` 共 1,651 页。当前结果使用 release 二进制、`mineru-vlm` 协议、
`MinerU2.5-Pro-2605-1.2B`、4 个生成 worker，并固定 `--no-cache`。官方评测使用 quick-match 24 workers；
Edit distance 越低越好，TEDS 越高越好。

| 路径 | 文本 Edit | 公式 Edit | 表格 TEDS | 结构 TEDS | 阅读顺序 Edit |
|---|---:|---:|---:|---:|---:|
| **当前 V2（2026-08-21）** | **0.069660** | **0.102585** | **0.906068** | **0.937463** | **0.135750** |
| 历史 `mineru-vlm-v2`（2026-08-13） | 0.070586 | 0.094509 | 0.901686 | 0.930939 | 0.137773 |
| 历史 `mineru-vlm-2605-official` | 0.037728 | 0.093537 | 0.919973 | 0.948615 | 0.129622 |
| 历史 `mineru-vlm-2605-surpass-e1-full` | 0.036731 | 0.094840 | 0.906502 | 0.938830 | 0.128454 |

相对历史 `mineru-vlm-v2`，当前 V2 文本 Edit 改善 `0.000927`、表格 TEDS 提升 `0.004382`、结构
TEDS 提升 `0.006524`、阅读顺序 Edit 改善 `0.002023`，但公式 Edit 退化 `0.008076`。相对
`official`，当前结果在文本、公式、表格和阅读顺序上均较弱；这说明统一 runner 的当前默认协议能够完整、
稳定执行，但**不能据此宣称质量超过现有 official 基线**。

生成输出为 1,651 个 Markdown，非零返回码 0、空文件 0；其中 1 个文件仅含换行。官方 page match
1,651 页无 timeout fallback，TEDS 665 个样本无 timeout/error/exception。生成文件首末写入跨度
`2,024.45s`（约 `1.23 pages/s`）；官方页匹配约 `1,230s`，表格 TEDS 约 89s。该墙钟数据包含
服务排队与不同页面复杂度，仅作为本机实测，不替代独立端到端计时基准。

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
| pipeline | `:9001` 无监听；layout/OCR/formula 均缺 Remote 服务 | 未评测 |

## 6. 原始产物

OpenDataLoader-Bench 结果：

- `opensource/opendataloader-bench/prediction/uparser-native-v2-engine/evaluation.json`
  SHA-256 `6f6b384cd24c2ee22604f18581cf230ec1b3f241091c452fe07bbb65e376681b`
- `opensource/opendataloader-bench/prediction/uparser-native-v2-canonical/evaluation.json`
  SHA-256 `bac90c68791c80b01d95856dd1aa575e13b85a95fc079f6967bce42c9fdd2d90`
- `opensource/opendataloader-bench/prediction/uparser-mineru-vlm-v2/evaluation.json`
  SHA-256 `53d0be510a2f3edf66dd124ef96af2a962fe772eba86e9854542bb997c9c3f73`
- `opensource/opendataloader-bench/prediction/uparser-auto-v2/evaluation.json`
  SHA-256 `fab547629964fcbb5246b88e16a5b2dd2c486d4fb8ac5d1ce0bac20f48442d38`
- `benchmark/results/architecture_v2_20260821.json`：格式、策略和 preflight 延迟明细。
- `benchmark/results/architecture_v2_bench_a_20260821.json`：可提交的 Bench A 精确指标、运行参数、差值和原始产物哈希。

OmniDocBench 结果：

- `benchmark/results/architecture_v2_omnidoc_metric_20260821.json`（官方 metric result 原样副本）
  SHA-256 `dd299fa7d498fb6ddf26650b900afa5d00ffb1078f50a42fef7551fb4b35eccb`
- `benchmark/results/architecture_v2_omnidoc_summary_20260821.json`（官方 run summary 原样副本）
  SHA-256 `c44a39544e0934d178a93c4b72c881b2fc058e3f01904f170ec8b23ad39869d5`
- `benchmark/OmniDocBench/result/architecture-v2-20260821_quick_match_*.json`：嵌套评测仓库中的原始输出。
- `benchmark/omnidoc_pred/architecture-v2-20260821/`：1,651 个当前 V2 Markdown 预测。

上述目录受仓库既有 `.gitignore` 管理；本报告固化关键数字和哈希，原始产物保留在当前工作区供复核。

## 7. 发布判断

- V2 runner/native 保真：**通过**。
- 默认 renderer 切换：**不通过，继续使用 engine**。
- 当前 MinerU model-protocol：**质量容差通过**；旧速度记录作废，新性能以 `0.620810s/doc` 为准。
- 当前 MinerU OmniDocBench：**完整性和稳定性通过**；相对历史 `mineru-vlm-v2` 有增有退，相对
  `official` 基线整体仍弱，不能标记为质量领先。
- auto：**本语料端到端收益通过**；未达到“人工标注 G-R 已通过”的证据级别。
- PaddleX / Mode 3 pipeline：**环境阻塞，不能宣称通过**。
