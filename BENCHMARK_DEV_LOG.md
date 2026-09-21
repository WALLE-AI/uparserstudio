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

对应 `BENCHMARK_REPORT.md` Part B §1 结果表的评测设置。

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

### 2.4 根因排查:8087 端口混用的真实原因,以及迁移到 8094 后的重测

§2.1 一直只确认了"端点在两个后端间随机切换",但没有查过*为什么*——用户后续问起,才去实际排查:

```
lsof -i :8087
COMMAND     PID    USER   FD   TYPE     DEVICE SIZE/OFF NODE NAME
python   707941   gaojing  40u  IPv4 ...  TCP *:8087 (LISTEN)
python  3641946   gaojing  41u  IPv4 ...  TCP *:8087 (LISTEN)
```

**两个独立的 `vllm.entrypoints.openai.api_server` 进程同时绑定在 8087**:一个是 2026-08-13 启动、服务 `Qwen3.5-4B` 的旧进程(PID 3641946,一直没关);一个是后来(2026-08-17)才起、服务 `Qwen3.8-27B` 的新进程(PID 707941)。Linux 允许多个进程通过 `SO_REUSEPORT` 绑定同一端口,内核会在它们之间轮询分发新连接——这不是任何一方配置了负载均衡,纯粹是**两次独立的服务启动撞在了同一个端口上,谁都没检查端口是否已被占用**。不是 uparser/评测脚本的 bug,是环境本身的服务冲突。

排查后确认没有空闲 GPU 显存(8 张 A100 全部被现有进程占满)可以另起一份新副本,于是把 Qwen3.8-27B 迁到了专用端口 **8094**(用户手动重启的服务),经 8 次连续探测 `/v1/models` 确认全部返回纯 `Qwen3.8-27B`、无切换后,`gen_qwen_omnidoc.py` 的默认 `--base-url` 改成了 `http://127.0.0.1:8094/v1`,并在 8094 上重新跑了一遍全量 1651 页(结果见 `BENCHMARK_REPORT.md` Part B §1)。

**混用 vs 纯净结果对比**(证实混用确实拖累了分数,尤其是 Table):

| | 混用 8087(旧) | 纯净 8094(新) | Δ |
|---|---|---|---|
| Text Edit↓ | 0.0596 | **0.0481** | -0.0115(更好) |
| Formula Edit↓ | 0.1373 | 0.1614 | +0.0241(变差,见下方说明) |
| Table TEDS↑ | 0.7161 | **0.7920** | **+0.076** |
| Table TEDS-S↑ | 0.7422 | 0.8259 | +0.084 |
| Reading Order↓ | 0.1573 | **0.1522** | -0.0051(更好) |

按之前(§3.4 之前的批次做过的)后端拆分分析,Qwen3.5-4B 单独的 Table TEDS 只有约 0.53,远低于 Qwen3.8-27B 单独的 0.81+——混用时约一半请求被答得更差的小模型接走,直接把整体 Table TEDS 拉低了 0.076。Formula Edit 反而变差不是模型退步,是纯 27B 服务本身处理慢(无并发优化,180s 超时的失败率从 0.12% 升到 1.0%),推测复杂公式页面在样本里的占比因失败模式变化而改变,不是同一批页面的可比结果——这个解释是推测,没有做逐类目验证,留作后续如果需要精确归因时的待办。

**已知的未回填差距**:§3 的 prompt 改进实验(B/C/BC 三个变体)全部是在混用期跑的,baseline 用的是旧的 0.7161,还没有在 8094 纯净端点上重新验证过"官方基线 prompt 优于变体 B/C/BC"这个结论是否仍然成立——纯净端点下模型本身更强,新增指令造成的"简单案例被做坏"这种病理是否还会以同样的幅度出现,是一个开放问题,如果要继续 prompt 实验这条线,应该先在 8094 上重新拿一个干净的基线,而不是直接拿混用期的数字做参照。

---

## 3. Part B(OmniDocBench):Prompt 改进实验全过程(结论见 `BENCHMARK_REPORT.md` Part B §2)

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

### 3.5 结论与建议(混用端点时期)

- **变体 B / C / BC 均不采纳**,§8 的官方基线 prompt 继续作为 Qwen3.8-27B 在 OmniDocBench 上的推荐配置(全量 Table TEDS 0.7161,优于三个变体)。
- 变体 D(结构化中间产物)/E(few-shot)/F(公式规范)按计划本应作为消融/次优先级实验,鉴于 B/C/BC 已经在主要目标指标上全量净负,**没有继续跑**——继续在同一套"通篇统一指令"思路上加码大概率重复同一种失败模式(小样本类目获益、其余类目受损),值得先改变思路而不是加变体。
- **对"改善 prompt 能否提高精度"这个问题的诚实回答**:对 Qwen3.8-27B 这个约 27B 的通用对话模型而言,**在 zero-shot 单轮 prompt 里塞入更具体的结构化指令,整体是净负的**——模型没有足够稳定的指令遵循能力去"只在需要时"应用复杂规则,反而把简单案例做坏的量超过了疑难案例获益的量。这与更强模型(如榜单上的 Qwen3-VL-235B)、或专门训练过版面语法的模型(mineru-vlm)的经验不能类比。
- 更有希望的方向(留作后续):
  1. **按文档复杂度做条件化 prompt**(而不是对所有页面用同一份更复杂的 prompt)——用本仓库已有的 `uparser classify`/profiler 先判断页面是否版面复杂,只在复杂页面上追加结构化指令,简单页面维持极简 prompt。这直接对应本节发现的病灶(复杂指令伤害简单页面)。
  2. 子集抽样需要覆盖**所有** `page_attribute` 取值(尤其 `watermark`/`fuzzy_scan`/`language` 各值),不能只挑"看起来困难"的类目,否则任何后续 prompt 实验都可能重蹈本次子集判断反转的覆辙。
  3. 若要继续这条路,应该先用一个能验证"指令是否被合理选择性应用"的小样本人工检查(读几个 watermark 类目的实际输出,而不是只看聚合分数),而不是直接扩大到全量再回头查因——这次是先跑全量才发现问题,浪费了一整轮全量评测的时间/请求量。

### 3.6 纯净端点(8094)复测:同一实验重跑一遍,结论不变(§2.4 留的"已知缺口"已回填)

`BENCHMARK_DEV_LOG.md` §2.4 记录过一个开放问题:§3.1–§3.5 的整套实验是在混用端点(8087)上跑的,不确定纯净端点(8094,真正的 Qwen3.8-27B,无 4B 污染)下同样的结论是否成立。用户后续要求补测,于是把 §3.2–§3.3 的整套流程(290 页子集筛选 → 全量确认)在 8094 上完整重跑了一遍。

**子集(290 页)结果**(baseline 直接从 0.6781 跳到 0.8280,证实纯净模型本身就强很多——见 `BENCHMARK_REPORT.md` §1 的根因说明):

| | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order↓ |
|---|---|---|---|---|---|
| baseline(纯净子集) | 0.0533 | 0.2286 | 0.8280 | 0.8686 | 0.1443 |
| 变体 B | 0.0469 | 0.2091 | **0.8609(+0.033)** | 0.8993 | 0.1409 |
| 变体 C | 0.0577 | 0.2299 | 0.8421(+0.014) | 0.8810 | 0.1508 |
| 变体 BC | 0.0478 | 0.1984 | 0.8471(+0.019) | 0.8917 | 0.1402 |

这次变体 B 单独是子集上唯一"四项指标全部同时变好"的,且 Table TEDS 提升幅度最大(+0.033,过了 §4.3 的 0.03 门槛)——C 单独反而拖累文本/公式/阅读顺序,BC 组合虽然公式最好但表格提升不如 B 单独,和混用端点时期"BC 组合最优"的模式不一样。选 B 单独进全量确认。

**全量(1651 页)确认结果——子集的乐观判断再次没有兑现**,不过这次不是"反转变负"而是"打平略负",幅度比混用端点时小得多:

| | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order↓ |
|---|---|---|---|---|---|
| baseline(纯净全量,`BENCHMARK_REPORT.md` §1) | 0.0481 | 0.1614 | **0.7920** | 0.8259 | 0.1522 |
| 变体 B | 0.0512 | 0.1630 | 0.7876(**-0.0044**) | 0.8153 | 0.1536 |

按类目拆解 Table TEDS(baseline → 变体 B,`table.page.TEDS` 口径,用于展示方向而非精确幅度——与官方 `table.all.TEDS.all` 聚合方式不同,不直接相加验证总量):同样的模式第三次出现——`layout_hard`(+0.124)、`book`(+0.097)、`note`(+0.093)、`table_hard`(+0.036)这些难例类目继续获益,`magazine`(-0.108)、`fuzzy_scan`(-0.094)、`watermark`(-0.045)、`newspaper`(-0.036)这些简单类目继续受损。区别是这次 `with_watermark`/`geometric_deformation`/`layout_three_column` 三个类目的 Δ 都是 **0.0000**(完全没被指令带偏)而不是像混用端点时那样暴跌——纯净的、更强的 Qwen3.8-27B 对"要不要套用复杂指令"这件事本身的判断力更好,但仍不足以让简单类目的整体收益转正,受损类目的总量级和获益类目基本抵消,net 结果打平偏负。

**最终结论(混用端点、纯净端点两次独立验证,方向一致)**:Qwen3.8-27B(不论是否受端点污染)在 OmniDocBench 上,给 prompt 加更具体的表格/版面结构化指令都不构成一个可采纳的净提升——官方基线 prompt 保持作为推荐配置。§3.5 的"更有希望的方向"建议(按复杂度条件化 prompt、子集需覆盖全部 page_attribute)在纯净端点上依然适用,尚未执行。

### 3.7 复现

```bash
cd benchmark
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost

# 子集实验(先于全量,便宜)——默认 --base-url 已指向纯净端点 8094
python3 select_omnidoc_subset.py
python3 gen_qwen_omnidoc.py --name subset_baseline_pure   --prompt-variant baseline    --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 gen_qwen_omnidoc.py --name subset_variant_b_pure  --prompt-variant variant_b   --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 gen_qwen_omnidoc.py --name subset_variant_c_pure  --prompt-variant variant_c   --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 gen_qwen_omnidoc.py --name subset_variant_bc_pure --prompt-variant variant_bc  --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json
python3 summarize_omnidoc.py subset_baseline_pure subset_variant_b_pure subset_variant_c_pure subset_variant_bc_pure

# 全量确认(贵,只在子集通过门槛后才跑)
python3 gen_qwen_omnidoc.py --name qwen3.8-27b-pure-variant-b --prompt-variant variant_b
python3 summarize_omnidoc.py qwen3.8-27b-pure qwen3.8-27b-pure-variant-b

# 混用端点(8087,已废弃)时期的历史复现命令,仅作记录:
python3 gen_qwen_omnidoc.py --name subset_baseline    --prompt-variant baseline    --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json --base-url http://127.0.0.1:8087/v1
python3 gen_qwen_omnidoc.py --name subset_variant_bc  --prompt-variant variant_bc  --dataset OmniDocBenchData/omnidoc_subset_prompt_experiment.json --base-url http://127.0.0.1:8087/v1
python3 summarize_omnidoc_by_backend.py subset_baseline subset_variant_bc
```

---

## 4. 两榜（opendataloader-bench + OmniDocBench）：monkeyocr-v2 与上游实现对齐的全过程

对应 `UPARSER_LEADERBOARD.md` 里 monkeyocr-v2 两行的更新，以及 `MONKEYOCR_V2_ALIGNMENT_PLAN.md`。

### 4.1 起因：差距在适配器，不在模型

本地 `127.0.0.1:8011` 跑的是官方 `MonkeyOCRv2-B-Parsing` 权重，而该模型在官方 README 的
OmniDocBench v1.6 端到端榜上以 **83.3 排第一**。我们同 harness 下却是：
opendataloader-bench Overall 0.8754（垫底、8.639 s/篇）、OmniDocBench Text Edit 0.1408。
同一份权重、同一个榜单，差距只能来自适配器。

于是逐行比对 `opensource/MonkeyOCRv2/parsing/core_runner.py`，确认 8 处偏差（完整清单见
`MONKEYOCR_V2_ALIGNMENT_PLAN.md` §2）。其中两处是主因。

### 4.2 主因一：识别阶段把裁剪图放大了约 10 倍，直接诱发模型复读

上游 `batch_inference` 对识别调用**不传** `min_pixels`，只继承 `MOCR2_MAX_PIXELS=1003520`
作为上界 —— `load_image` 的 `min_pixels` 只上采样、`max_pixels` 只下采样，所以一个
400×30 的文本行裁剪是**原样**送进模型的。我们当时两个阶段都用 `min == max == 1003520`，
把这个 12000 像素的小图 Lanczos 放大到约 100 万像素再送。

这不是"精度略有差异"，而是直接把模型打进复读循环。一个真实样本
（`yanbaopptmerge_yanbaoPPT_90`，GT 只有一个 title + 一张图）：

| | 输出长度 | 内容 |
|---|---|---|
| 对齐前 | 45099 字符 | `## 爆竹声中——岁除` 之后是 `The text content from the image is:` 连续重复数百次 |
| 对齐后 | 48 字符 | `## 爆竹声中一岁除` + 几个拼音/拆字块，与 GT 一致 |

这类复读输出还有一个次生后果：把评测器本身拖垮。OmniDocBench 的 `quick_match` 对该样本
统计到 `gt=1 pred=1676`，单页匹配耗时 1735 秒；用旧预测跑 290 页子集时评测器在 40 分钟
超时上限内**跑不完**，一个汇总数字都没产出。所以"对齐前 vs 对齐后"的子集 A/B 并不是我
主动放弃的，而是旧输出烂到评测器无法收敛——这本身就是结论的一部分。

### 4.3 主因二：OTSL 分词把单元格里的任意标签当成了控制符

上游的分词正则是 `<(fcel|ecel|lcel|ucel|xcel|nl)>`，白名单只有六个控制标签，源码注释写得很
直白："Other markup (e.g. `<br>` or a nested `<table>`) is cell content and must remain
untouched."。我们的移植用的是 `<([a-z]+)>` —— 任意小写标签都被吞成控制符。于是单元格里
一个 `<br>` 就会让该行之后的每个单元格整体左移一列，整张表的网格结构报废。

连带确认并修正的还有：`<otsl>…</otsl>` 包裹未剥离、`html2otsl` 的私有转义（U+E100）未解码、
`fcel` 内容被 `.trim()`（上游明确不做）、单元格转义用的是会把 `&amp;` 二次转义成
`&amp;amp;` 的共享 `otsl::escape_html`（上游是"保留内嵌 HTML 标签、只转义纯文本块，且 `&`
带实体负向 lookahead"）。

### 4.4 golden 值一律由执行上游 Python 得到

沿用本仓库既有纪律：`otsl_to_html` / `detect_repeat_token` / `process_formula` 的期望值全部
由 `python3 -c 'import core_runner'` 真跑上游函数抓取，而不是读代码推断。这次因此逮到两个
**靠推理一定会写错**的上游 quirk，两个都被刻意复现而非"修正"：

1. 分词大小写不敏感，但 `tag == 'fcel'` 的分支判断是大小写敏感的 —— 所以 `<FCEL>up<NL>`
   匹配得上正则、却落到最后的 `else`，列号前进但不产出任何单元格，结果是
   `<table><tr></tr></table>`。
2. 单元格内容按 `(<[A-Za-z][^>]*>)` 切片，要求 `<` 后紧跟**字母**，于是**闭合**标签
   `</b>` 不匹配、被当作纯文本转义。真实输出里 `<b>bold</b>` 渲染成
   `<b>bold&lt;/b&gt;`。

### 4.5 重复重试：换成上游算法，并按上游默认**关闭**

上游 `detect_repeat_token` 是后缀重复法（`base_max_repeats=4`、`window_size=500`、
`scaling_factor=3.0`），允许的重复次数随重复单元长度递减：单字符要 17 次才算退化，
12 字符短语 5 次就算。再补一次"去掉末尾 50 字符"的二次判定。我们原先用的是自研的
`robustness::is_degenerate`（滑窗周期法），两者在真实输出上判断并不一致。

另外上游 `PipelineConfig.retry_repeat` 默认是 **False**，我们却默认开启。按用户选择的
"完全对齐"口径，现在默认关闭，并新增 `--monkeyocr-retry-repeat` /
`--monkeyocr-retry-repeat-max-retries` 显式开启；开启时对**所有** `need_infer` 标签生效
（含 Table/Formula，上游如此），温度 `min(0.2*(n+1), 0.8)`、`top_p=0.95`。
`robustness.rs` 本身未改动，mineru-vlm 仍在用。

> 温度常量特意用 `f64` 而非 `f32`：上游是 Python float，第三次重试的值是
> `0.6000000000000001`；用 `f32` 会在序列化时变成 `0.20000000298023224` 这种放大误差。

### 4.6 评测过程中踩到的两个坑（与适配器无关，但会让数字不可信）

1. **本机 wiremock 单测被代理拦截**。`cargo test --workspace` 一开始有 13 个 transport/
   semantic 用例失败，错误是 nginx 的 404 页面 —— 请求本地 wiremock 端口时走了公司代理。
   加 `NO_PROXY=127.0.0.1,localhost` 后 510 个用例全绿。与本次改动无关，是环境问题。
2. **OmniDocBench 评测器自身的 `RecursionError`**。`_build_formula_partitioned_pred_candidates`
   的 `backtrack` 是无界递归，公式候选够多的页面会超过 CPython 默认 1000 帧上限，整次评测
   在写出任何指标前就崩掉。新增 `benchmark/OmniDocBench/run_eval_deep.py`：只抬高
   `sys.setrecursionlimit` 与 `threading.stack_size`（后者必须在匹配线程池创建前设置，否则
   更深的递归不是抛异常而是段错误），不触碰匹配/打分/配置。未触发上限的样本两个入口结果
   逐字节一致，所以用它评测的预测集与用 `run_eval.py` 评测的仍可比。
3. **CDM 曾静默返回 0**。第一次全量评测把 TeX 根目录猜成 `/home/dataset1/gaojing/texlive`，
   实际是 `.../texlive/2026`，`pdflatex` 不存在，CDM 对每个样本恒返回 0（榜单说明里早有
   这条注记）。用正确路径重跑后才拿到真实 CDM 值。

### 4.7 子集不作数，全量才作数

本文件 §3.3 已经记录过一次教训：290 页子集曾给出与全量 1651 页**相反**的结论。所以这次
子集只当"方向闸门"，最终写进榜单的全部是全量 1651 页（OmniDocBench）和全量 200 篇
（opendataloader-bench）的数字，且两个榜单的"对齐前"一侧都是同一 harness 重新评测的，
不是引用旧文档。

### 4.8 复现

```bash
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost

# OmniDocBench v1.6 全量 1651 页（uparser CLI 逐页出 markdown）
cd benchmark
ls OmniDocBenchData/images/* | xargs -P 8 -I{} ./gen_monkey_aligned_full.sh {}
cd OmniDocBench
TL=/home/dataset1/gaojing/texlive/2026
PATH="$TL/bin/x86_64-linux:$PATH" CDM_TEXLIVE_ROOT="$TL" \
  CDM_PDFLATEX="$TL/bin/x86_64-linux/pdflatex" \
  .venv/bin/python run_eval_deep.py \
  --config configs/omnidoc_uparser-monkeyocr-v2-aligned-full-20260918.yaml

# opendataloader-bench 全量 200 篇
cd opensource/opendataloader-bench
uv run src/pdf_parser.py --engine uparser-monkeyocr-v2
uv run src/evaluator.py
```

> 注意 `ls OmniDocBenchData/images/*` 不要写成 `*.png`：该数据集 1651 页里 981 页是 `.jpg`，
> 只匹配 `.png` 会静默只跑 670 页。

---

## 5. OmniDocBench：monkeyocr-v2 第二轮——"是不是后处理的问题"的定量回答

对应 `MONKEYOCR_V2_ALIGNMENT_PLAN.md` §8。

### 5.1 先收回一个错误结论

§4 和榜单初稿都写了"官方 README 的 OmniDocBench v1.6 端到端榜上 MonkeyOCRv2-B-Parsing
以 83.3 排第一"。**这是错的**。重新核对 README：83.3 属于 §6 的 **MDPBench**（多语言文档
解析），不是 OmniDocBench；README 里唯一的 OmniDocBench 1.6 数字在 §2，是**独立的公式识别
子模型**在**真值公式裁剪图**上的成绩（CDM 90.8 / ExpRate 61.1）。**官方从未公布
MonkeyOCRv2 的 OmniDocBench 端到端解析成绩**，所以"与官方精度对齐"在这个榜单上没有靶子，
可比的参照只有本仓库榜单里的其他引擎。§4 的因果结论（差距来自适配器）不受影响。

### 5.2 先排除 `monkeyocr_post.rs`

上游仓库自带 `parsing/tests/`，本地 pytest 直接跑通（39 passed）。把 `test_otsl_to_html.py`
的 16 个用例与 `test_repeat_detection.py` 的 6 个参数化用例逐条搬进 Rust 测试，**一次通过，
实现零改动**——包括作者自己标注的 "#24 行内换行被截断" 回归，和 `train/html2otsl.py`
产出的跨行跨列矩阵（`<fcel>Big<lcel><fcel>A<nl><ucel><xcel><fcel>B<nl>` →
`rowspan="2" colspan="2"`）。协议层与内容后处理层不是剩余差距的来源。

### 5.3 A/B：同一份模型输出，只换组装方式

跑 `--format json --no-postprocess` 拿 1651 页原始 block，用忠实移植 `result2md` 的脚本
（`benchmark/result2md_upstream.py`）重新组装。两侧模型输出同源，唯一变量是组装：

| 指标 | 共享渲染器 | 上游 `result2md` |
|---|---:|---:|
| Text Edit ↓ | 0.0834 | **0.0498** |
| Table Edit ↓ | 0.3967 | **0.1040** |
| Formula Edit ↓ | 0.1990 | **0.1592** |
| Reading Order Edit ↓ | 0.1549 | **0.1328** |

成因三条：表格被降级成管道表（Table Edit 那 −0.29 几乎全在这里）、注入 `- ` 列表标记
（189/1651 页）、Markdown 元字符转义（509/1651 页）。

### 5.4 一个被自己实验否掉的猜测

先验上很像主因的一条：`content_normalize` 把 CJK 文本里的半角标点统一成全角，而真值里
**27.3% 的中文文本块本来就含半角标点**（9332 个字符会被改写，861/1651 页输出因此不同）。
跑第三个变体（上游组装 + 我们的标点归一化）全量确认：Text Edit **0.0498 → 0.0504**，
只有 +0.0007。**统计上的"改写机会"不等于评测代价**。因为做了这个实验，才没有据此去动
共享模块（那会影响所有协议并需要重测整张榜）。

### 5.5 Rust 实现与脚本重建的一致性校验

实现后没有直接信全量数字，而是先在样本上把真实 CLI 输出与 Python 重建逐字节比对：
差异全部落在模型自己的 LaTeX/文本空格习惯上（`\varrho=+1` vs `\varrho = +1`、
`R module` vs `R-module`），相似度 0.97–0.9996，**结构完全一致**——即两次跑的模型
非确定性，而不是组装分歧。之后全量重测（真实 CLI）得到 Text Edit 0.0499，与重建的
0.0498 吻合。

### 5.6 复现

```bash
cd benchmark
# 1) 原始 block（供重建用）
ls OmniDocBenchData/images/* | xargs -P 8 -I{} ./gen_monkey_json.sh {}
# 2) 忠实重建 & 变体
python3 result2md_upstream.py omnidoc_pred_json/monkey-aligned omnidoc_pred/monkey-upstream-assembly-20260920
python3 variant_normalize.py  omnidoc_pred_json/monkey-aligned omnidoc_pred/monkey-upstream-plus-normalize-20260920
# 3) 真实 CLI 全量（榜单数字来源）
ls OmniDocBenchData/images/* | xargs -P 8 -I{} ./gen_monkey_assembly.sh {}
# 4) 评测（三者同一 config 模板，只改 data_path）
cd OmniDocBench && TL=/home/dataset1/gaojing/texlive/2026 \
  PATH="$TL/bin/x86_64-linux:$PATH" CDM_TEXLIVE_ROOT="$TL" CDM_PDFLATEX="$TL/bin/x86_64-linux/pdflatex" \
  .venv/bin/python run_eval_deep.py --config configs/omnidoc_uparser-monkeyocr-v2-assembly-20260920.yaml
```
