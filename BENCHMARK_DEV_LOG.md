# uparser 评测研发日志

本文件收纳 `BENCHMARK_REPORT.md` 里**不属于榜单结果本身**的内容:调试过程、探索性发现、失败的尝试、根因排查细节。`BENCHMARK_REPORT.md` 只保留干净的多模型/多数据集榜单结果表和结论;想知道某个数字是"怎么来的"、中间踩过什么坑,来这里查。

---

## 1. Part A(opendataloader-bench):mineru-vlm 渲染缺陷的发现与修复

对应 `BENCHMARK_REPORT.md` Part A §1 结论表里 mineru-vlm 的"修复后"标注。

### 现象
首轮 mineru-vlm 跑分:Overall **0.7080**,其中 nid=0.947、teds=0.944 均为全场最佳,但 **mhs=0.0000**(200 篇里仅 1 篇有 `#` 标题)。

### 根因
`render/mod.rs::to_markdown` 对每个文本 block **直接输出 `block.text`,完全忽略 `block.category`**。于是 mineru-vlm 明明把标题正确分类为 `title`、列表分类为 `list`,渲染成 markdown 时却全部退化成普通段落——一个 `#` 都不出。而 heading 指标(MHS)依赖 markdown 的 `#` 结构,于是被清零,把一个本应第一的引擎拖到 0.708。

> 对照:`native` 的 markdown 直接用内嵌引擎(pdf-inspector)自带的 markdown(本就带 `#` 标题),所以 native 不受此缺陷影响——这也是"抽取更强的 mineru-vlm 反而 Overall 更低"这一反直觉现象的真正原因。

### 修复
在 `to_markdown` 里按归一化类别加 markdown 标记:`title → "# "`、`list → "- "`(`header` 是页眉,非内容标题,不加)。改动**仅影响 VLM 协议**(mineru-vlm/dots.ocr/monkeyocr-v2/pipeline/paddleocr 共享此渲染器),**不触碰 native**(其 markdown 走引擎自有 pipeline)。

### 效果

| mineru-vlm | Overall | mhs | 含 `#` 标题的文档数 |
|---|---|---|---|
| 修复前 | 0.7080 | 0.0000 | 1 / 200 |
| **修复后** | **0.9284** | **0.8777** | **112 / 200** |

单一渲染层修复带来 **+0.22 Overall**,并让 mhs 从垫底跃居第一(0.878 > hybrid 0.821)。这是一个"正确代码从未接入真实渲染路径"类型的缺陷,与本项目此前多次记录的同类问题一脉相承。

---

## 2. Part B(OmniDocBench):Qwen3.8-27B 评测的背景排查

对应 `BENCHMARK_REPORT.md` Part B §8 结果表的评测设置。

### 2.1 背景与偏离说明

用户要求评测 `127.0.0.1:8087` 上的 "Qwen3.8-27B" 在 OmniDocBench 上的精度。执行过程中发现两个必须先确认才能保证结果可信的问题(均已与用户确认后继续):

1. **该端点的模型 id 不稳定**:反复探测 `/v1/models` 发现它在两个不同后端之间随机切换——`Qwen3.5-4B`(`max_model_len=16384`,id 无前导字符)和 `Qwen3.8-27B`(`max_model_len=32000`,id 带一个 U+00A0 不换行空格前缀,不是普通空格)——像是负载均衡到了两个独立部署,而非单一稳定模型。用户明确指示"继续用这个端口,接受混合"。因此 Part B 的结果**是这两个后端混合产出的,不是纯 Qwen3.8-27B 的分数**;生成脚本(`benchmark/gen_qwen_omnidoc.py`)在**每次请求前**都重新 `GET /v1/models` 取当前存活的 model id 再发请求,避免用一个失效的旧 id 硬编码导致整批 404。
2. **Qwen3.8-27B 不是文档解析专用模型**,不匹配 uparser 任何一个 protocol adapter 训练过的输出语法(MinerU 的 `custom_token`、dots.ocr 的 strict-JSON 等)——套用现成 adapter 会导致解析器把模型的自由格式输出当成畸形数据大量丢弃。用户选择"自定义 prompt 方案"。**没有从零设计 prompt**,而是直接复用 OmniDocBench 官方仓库自带、专为通用 VLM baseline 准备好的参考 prompt(`benchmark/OmniDocBench/tools/model_infer/Qwen3-VL-235B_img2md.py` 与 `gpt_5.2_img2md.py` 两个官方脚本共享的同一份 prompt)——文本原样转录、行内公式 `\( \)`/块级公式 `\[ \]`、表格 `<table>` HTML、忽略图片——这套约定是官方评测器 `quick_match` 本就按此解析的,不是本项目自造、需要自证有效性的格式。

### 2.2 一个真实发现:必须显式关闭 thinking,否则思维链会污染输出

冒烟测试阶段(先跑 6–12 页,再跑全量)发现:至少有一个后端在默认设置下把完整思维链(如 `"The user wants me to convert an image..."` 开头的分析性文字,长达数十行)直接写进了 `message.content`,而不是分离到独立的 `reasoning`/`reasoning_content` 字段——这会让该页的 Markdown 输出前几十行全是英文分析文字而非文档内容,Edit_dist 会被打得很惨,而且**不会报错**,是那种"看起来正常跑完、分数却被污染"的静默错误,和这个项目历史上多次记录的问题是同一类。排查后确认:在请求体里加 `"chat_template_kwargs": {"enable_thinking": false}` 能让两个后端都干净地只输出最终 Markdown(`reasoning` 字段变为空字符串,`content` 直接是纯 Markdown),因此该参数被硬编码进了生成脚本,**在跑全量之前用相同的问题页面验证过修复生效**,而不是假设它有效就直接跑。

### 2.3 生成过程与失败率

- 新增独立脚本 `benchmark/gen_qwen_omnidoc.py`(不复用 `run_uparser_omnidoc.py`,因为那个脚本是走 `uparser` CLI 的 protocol adapter,这里没有 adapter 可用)——直接对 `http://127.0.0.1:8087/v1/chat/completions` 发请求,`temperature=0`,6 个并发 worker,单页最多重试 8 次(对端点闪断/model-id 竞态导致的 404 用 0.3s 快速重试,而不是长退避——等待不会让竞态窗口变窄)。
- 全量 1651 页(§8 基线):首轮生成 1607 个待处理页(44 页来自更早的冒烟测试已存在),22 页(1.3%)在 8 次重试后仍失败(21 个 404 model-id 竞态 + 1 个 400 Bad Request);对这 22 页做了第二轮重试,最终**仅 2/1651(0.12%)页保持空预测**(1 个持续 404、1 个持续 400)——按官方评测器的既定口径,空预测按空字符串参与打分,不做人工剔除或补分。
- 本环境**没有 CDM 所需的 TeXLive/`pdflatex`/`kpsewhich`**(探测返回 `unavailable: FileNotFoundError`),因此公式列只有 `Edit_dist`,没有官方 Formula<sup>CDM</sup> 分数,也**不计算官方定义的单一 Overall 复合分**(该分需要 CDM)——与本仓库 `summarize_omnidoc.py` 对 `native`/`mineru-vlm-2605-surpass-e1-full` 历史跑分的呈现口径一致,只报四项原始指标。

---

## 3. Part B(OmniDocBench):Prompt 改进实验全过程(结论见 `BENCHMARK_REPORT.md` §9)

按 `QWEN_PROMPT_IMPROVEMENT_PLAN.md` §3/§4 执行,沿用 §2 的评测设置(同一端点、同一评测器)。

### 3.1 做了什么

在官方基线 prompt 上做了两处增量修改并各自单独/组合测试:

- **变体 B**:把"Table Processing"一节从一句话换成显式的合并单元格(rowspan/colspan)、空单元格保留、多行表头处理指令。
- **变体 C**:追加一节"Multi-Column Layout",要求多栏页面按列(而非按行)转录。
- **变体 BC**:B + C 组合。

代码改动:`gen_qwen_omnidoc.py` 拆出 `PROMPTS` 字典 + `--prompt-variant` 参数(定义了 B/C/BC/D/E/F/BCF 共 7 个变体,本轮只跑了 B/C/BC,D/E/F 未跑——见 §3.5);每次请求记录服务端实际应答的 `model` 字段到 `<name>_backend_map.json`,用于按后端拆分复核(`summarize_omnidoc_by_backend.py`,新增);新增 `select_omnidoc_subset.py` 按官方 `page_attribute` 标签(`table_hard`/`layout_hard`/`newspaper`/`academic_literature`/多栏 + 50 页随机对照)分层抽样出一个 290 页子集,用于在跑全量 1651 页之前先低成本筛选候选。

顺带发现一个**误报**:检查预测文件时发现约 44%(731/1651)的 §8 基线输出被 ` ```markdown ... ``` ` 代码围栏包裹,一度以为这是拖累 §8 分数的未被发现的 bug,补了一个通用的 `strip_code_fence()` 并清理了已有文件——但重新跑官方评测器后分数**逐位不变**,查证后确认 OmniDocBench 官方评测器的 `data_preprocess.py::remove_markdown_fences` 早就在内部做了同样的归一化。**这不是本项目的 bug,清理是无害的冗余工作**,记录于此避免以后重复排查同一件事。

### 3.2 子集实验(290 页,偏向困难类目)结果——B/C/BC 看起来都显著有效

| 变体 | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order↓ |
|---|---|---|---|---|---|
| baseline(子集) | 0.0724 | 0.2239 | 0.6781 | 0.7105 | 0.1614 |
| 变体 B | 0.0675 | 0.1996 | 0.7399(+0.062) | 0.7670 | 0.1600 |
| 变体 C | 0.0833 | 0.1866 | 0.7474(+0.069) | 0.7784 | 0.1673 |
| **变体 BC** | **0.0639** | 0.2073 | **0.7731(+0.095)** | **0.8104** | **0.1560** |

按计划 §4.3 的门槛(Table TEDS 提升 ≥0.03、护栏指标不明显恶化)三个变体都通过,BC 全指标最优。按计划 §4.2 又做了后端拆分复核(`summarize_omnidoc_by_backend.py`),确认 BC 在两个后端上 Table TEDS 提升方向一致(Qwen3.5-4B:0.5681→0.5986,+0.030;Qwen3.8-27B:0.8370→0.8728,+0.036)——不是"恰好被强后端多回答了几次"的抽样假象。**至此看起来是一次成功的 prompt 改进。**

### 3.3 全量 1651 页确认跑——结果反转,子集判断是错的

按计划 §4.4 步骤 4,选 BC 在全量 1651 页上做最终确认,结果与子集**方向相反**:

| 全量(1651 页) | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order↓ |
|---|---|---|---|---|---|
| baseline(§8) | 0.0596 | 0.1373 | **0.7161** | 0.7422 | 0.1573 |
| 变体 B | 0.0601 | 0.1450 | 0.6827(**-0.033**) | 0.7076 | 0.1625 |
| **变体 BC** | 0.0565 | 0.1400 | 0.6668(**-0.049**) | 0.6917 | 0.1592 |

为排除"是不是只有变体 C 的多栏指令在全量上有害"这个假设,额外把**变体 B 单独**也跑了一次全量确认——结果 B 单独同样净负(-0.033),说明问题不止在 C,变体 B 自己的表格结构化指令在全量分布上也是净负贡献。

### 3.4 根因:子集抽样有覆盖盲区,新增指令让模型在"简单/非常规"表格页面上过度纠结

按类目拆解全量 Table TEDS 的逐类变化(baseline → 变体 B):

| 类目 | baseline | 变体 B | Δ |
|---|---|---|---|
| `with_watermark` | 0.9568 | 0.4298 | **-0.527** |
| `language: traditional_chinese` | 0.8101 | 0.5887 | -0.221 |
| `fuzzy_scan` | 0.9749 | 0.8278 | -0.147 |
| `data_source: magazine` | 0.8496 | 0.7101 | -0.140 |
| `watermark` | 0.9586 | 0.8260 | -0.133 |
| `data_source: exam_paper` | 0.9243 | 0.8360 | -0.088 |
| `data_source: PPT2PDF` | 0.9747 | 0.8896 | -0.085 |
| … | | | |
| `subset: layout_hard` | 0.6959 | 0.9052 | **+0.209** |
| `geometric_deformation` | 0.7072 | 0.9916 | **+0.285** |

模式很清楚:新增的"识别合并单元格/多行表头/空单元格"指令,在**本来就困难**的类目(`layout_hard`、`geometric_deformation`)上确实让模型更仔细,分数大涨;但在**本来就简单、表格规整**的类目(水印、扫描模糊件、PPT 转 PDF、杂志)上,同样的指令反而让模型**过度分析**——开始给不需要 rowspan/colspan 的规整表格强行加合并标记、拆分本不该拆的单元格,把原本 0.95+ 的高分表格做坏。`with_watermark` 类目暴跌 0.53 是最极端的例子。

而 §3.1 描述的 290 页子集,分层标签只覆盖了 `table_hard`/`layout_hard`/`newspaper`/`academic_literature`/多栏 + 50 页无差别随机对照,**完全没有专门覆盖 `watermark`/`fuzzy_scan`/`traditional_chinese` 这几个后来被证明受害最深的类目**——50 页随机对照池太小,没能采样到足够多这类页面来暴露问题。这不是"prompt 本身没用",而是**子集设计的覆盖盲区导致误判**,恰好印证了计划文档 §6 自己写的风险("子集分数不能线性外推到全量"),只是没想到会是反向的、而不只是幅度上的偏差。

### 3.5 结论与建议

- **变体 B / C / BC 均不采纳**,§8 的官方基线 prompt 继续作为 Qwen3.8-27B 在 OmniDocBench 上的推荐配置(全量 Table TEDS 0.7161,优于三个变体)。
- 变体 D(结构化中间产物)/E(few-shot)/F(公式规范)按计划本应作为消融/次优先级实验,鉴于 B/C/BC 已经在主要目标指标上全量净负,**没有继续跑**——继续在同一套"通篇统一指令"思路上加码大概率重复同一种失败模式(小样本类目获益、其余类目受损),值得先改变思路而不是加变体。
- **对"改善 prompt 能否提高精度"这个问题的诚实回答**:对 Qwen3.8-27B 这个约 27B 的通用对话模型而言,**在 zero-shot 单轮 prompt 里塞入更具体的结构化指令,整体是净负的**——模型没有足够稳定的指令遵循能力去"只在需要时"应用复杂规则,反而把简单案例做坏的量超过了疑难案例获益的量。这与更强模型(如榜单上的 Qwen3-VL-235B)、或专门训练过版面语法的模型(mineru-vlm)的经验不能类比。
- 更有希望的方向(留作后续,未执行):
  1. **按文档复杂度做条件化 prompt**(而不是对所有页面用同一份更复杂的 prompt)——用本仓库已有的 `uparser classify`/profiler 先判断页面是否版面复杂,只在复杂页面上追加结构化指令,简单页面维持极简 prompt。这直接对应本节发现的病灶(复杂指令伤害简单页面)。
  2. 子集抽样需要覆盖**所有** `page_attribute` 取值(尤其 `watermark`/`fuzzy_scan`/`language` 各值),不能只挑"看起来困难"的类目,否则任何后续 prompt 实验都可能重蹈本次子集判断反转的覆辙。
  3. 若要继续这条路,应该先用一个能验证"指令是否被合理选择性应用"的小样本人工检查(读几个 watermark 类目的实际输出,而不是只看聚合分数),而不是直接扩大到全量再回头查因——这次是先跑全量才发现问题,浪费了一整轮全量评测的时间/请求量。

### 3.6 复现

```bash
cd benchmark
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost

# 子集实验(先于全量,便宜)
python3 select_omnidoc_subset.py
python3 gen_qwen_omnidoc.py --name subset_baseline    --prompt-variant baseline    --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 gen_qwen_omnidoc.py --name subset_variant_b   --prompt-variant variant_b   --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 gen_qwen_omnidoc.py --name subset_variant_c   --prompt-variant variant_c   --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 gen_qwen_omnidoc.py --name subset_variant_bc  --prompt-variant variant_bc  --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 summarize_omnidoc.py subset_baseline subset_variant_b subset_variant_c subset_variant_bc
python3 summarize_omnidoc_by_backend.py subset_baseline subset_variant_bc

# 全量确认(贵,只在子集通过门槛后才跑)
python3 gen_qwen_omnidoc.py --name qwen3.8-27b-variant-b  --prompt-variant variant_b
python3 gen_qwen_omnidoc.py --name qwen3.8-27b-variant-bc --prompt-variant variant_bc
python3 summarize_omnidoc.py qwen3.8-27b qwen3.8-27b-variant-b qwen3.8-27b-variant-bc
```
