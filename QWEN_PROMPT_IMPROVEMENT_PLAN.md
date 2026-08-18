# Qwen3.8-27B × OmniDocBench:Prompt 改进执行方案

> 状态:**已执行完毕(变体 B/C/BC),结论为负——不采纳,§8 官方基线 prompt 继续作为推荐配置**。完整过程、子集与全量的反转、根因诊断见 `BENCHMARK_REPORT.md` §9。变体 D/E/F 按 §9.5 未跑(已在 B/C/BC 全量净负后判定继续同一思路价值有限)。
> 基线:`BENCHMARK_REPORT.md` §8(2026-08-18,全量 1651 页,官方参考 prompt)
> 目标:验证"改善 prompt 设计能否提高 Qwen3.8-27B 在 OmniDocBench 上的精度",并给出可执行、可回滚的实施步骤

---

## 1. 基线诊断:精度短板在哪

基线用的是 OmniDocBench 官方给通用 VLM baseline 用的参考 prompt(`Qwen3-VL-235B_img2md.py`/`gpt_5.2_img2md.py` 共用那份),没有针对 Qwen3.8-27B 或本数据集做任何定制。四项指标:

| 指标 | 基线值 | 相对参照 |
|---|---|---|
| Text Edit↓ | 0.0596 | 与官方 235B 通用 VLM(0.063)同量级,**不是短板** |
| Reading Order Edit↓ | 0.1573 | 与官方 235B(0.166)同量级,**不是短板** |
| Table TEDS↑ | 0.7161 | 明显落后本仓库 mineru-vlm(0.9065)、官方 235B(0.8307),**主要短板** |
| Formula Edit↓ | 0.1373 | 无 CDM 环境不可直接对比,但按同类模型经验偏高,**次要短板** |

按数据类别拆解 Table 指标(来自 `qwen3.8-27b_quick_match_metric_result.json`),定位到具体薄弱场景而不是笼统地说"表格差":

| 类别 | Table TEDS↑ | 解读 |
|---|---|---|
| `data_source: newspaper` | 0.6206 | 最差——报纸多栏+密集表格混排 |
| `data_source: academic_literature` | 0.6570 | 学术论文的三线表、跨栏表格 |
| `language: english` | 0.6861 | 英文表格弱于中文表格(0.784) |
| `subset: layout_hard` | 0.6959 | 官方标注的"版面困难"子集,符合预期 |
| `subset: table_hard` | 0.7235 | 官方标注的"表格困难"子集(合并单元格/嵌套表头) |
| `layout: 1andmore_column` | 0.7111 | 多栏版面里的表格 |
| 对照:`watermark`/`PPT2PDF`/`exam_paper` | 0.92–0.97 | 干净的单栏、规则表格已经很好 |

**结论**:模型不是"看不懂表格",而是在**多栏版面定位表格边界**和**合并单元格/嵌套表头的结构还原**上系统性弱——这正是 prompt 里当前只有一句"Convert tables to HTML format"能解决不了的问题,需要更具体的结构化指令。

---

## 2. 改进假设(每条都要能被数据推翻)

| # | 假设 | 依据 | 如果假,说明什么 |
|---|---|---|---|
| H1 | 显式描述"如何处理合并单元格"(rowspan/colspan 用法+示例)能提升 `table_hard`/`layout_hard` 子集的 TEDS | 当前 prompt 完全没提 rowspan/colspan,模型大概率靠预训练默认行为处理,不一致 | 说明瓶颈在视觉定位而非输出格式知识,需要别的手段(如两阶段/先定位后转录) |
| H2 | "先描述表格结构(行数/列数/合并区域),再输出 HTML"的两步式指令(单次请求内,不拆成两次 API 调用)能提升多栏/嵌套表格的 TEDS | 结构化中间产物是已知能提升 VLM 结构化输出准确率的技术(先分解后生成) | 说明模型的瓶颈是"看图"能力上限,prompt 端难以再撬动,需要模型/分辨率层面改动 |
| H3 | 明确要求"每栏独立按阅读顺序转录,栏与栏之间用分隔标记"能提升多栏报纸/学术论文的 Text Edit 和 Table 定位准确率 | `newspaper`/`academic_literature` 两个类别的 Table 和 Text 指标都偏差,提示多栏是共同病因 | 说明多栏本身对该模型分辨率/token 预算是硬限制 |
| H4 | 补充 1-2 个 few-shot 示例(表格 markdown 转录的输入输出对,文字描述而非真实图片,避免占用过多 token)能提升整体一致性 | 参考 prompt 目前是纯 zero-shot 指令,没有示例 | 说明当前失败模式不是"不知道格式要求"而是"看不清/结构判断错",few-shot 帮不上 |
| H5 | 公式部分补充"避免用 Unicode 数学符号,统一用 LaTeX 命令"+ 常见环境(`\begin{cases}`/`\begin{aligned}`/矩阵)的显式示例能降低 Formula Edit | 已观察到基线输出里公式用了 `\Rightarrow`、`\begin{cases}` 等,格式基本对,但没有被强制要求覆盖矩阵/多行环境等复杂结构 | 说明差距在识别而非格式规范,prompt 端收益有限 |

---

## 3. 候选 Prompt 设计(在官方基线基础上做增量,不推倒重来)

保留官方基线 prompt 的 1/2/4/5 节(文本处理、公式规则、图片忽略、输出格式)不变,**只替换第 3 节"Table Processing"**,并追加一节"多栏版面处理"——这样能把"改了哪部分、对哪个指标起作用"的归因做干净,而不是同时改一堆变量。

### 变体 B(表格结构化指令,验证 H1)

替换官方 prompt 第 3 节为:

```
3. Table Processing:
- Convert tables to HTML format, wrapped in <table>...</table>.
- Before writing the HTML, silently identify: the number of header rows,
  the number of data rows, and any cells that visually span multiple rows
  or columns (merged cells).
- Represent merged cells using rowspan/colspan attributes on the spanning
  <td>/<th> element; do NOT repeat the same value in multiple cells to
  fake a merge.
- Preserve empty cells as <td></td>, do not omit them or shift columns.
- If a table has a multi-row header (nested headers), use multiple <tr>
  inside <thead> with rowspan/colspan so column alignment stays correct.
```

### 变体 C(多栏版面处理,验证 H3)

在官方 prompt 末尾(第 5 节之后)追加:

```
6. Multi-Column Layout:
- If the page has 2 or more text columns (newspaper/magazine/academic
  paper style), transcribe strictly column-by-column, top-to-bottom
  within each column, left column before right column -- not row-by-row
  across columns.
- If a table or figure spans across columns, treat it as a single block
  positioned at its actual reading-order position, do not split it.
```

### 变体 D(结构化中间产物,验证 H2;与 B 组合使用)

在变体 B 基础上,把"silently identify"改成显式输出一段简短结构描述再输出 HTML(不是隐藏的静默步骤),验证"暴露中间推理"是否真的比"要求模型自己心算"更准:

```
3. Table Processing:
- For each table, first output one line: "TABLE STRUCTURE: R rows x C
  columns, merged cells: [(row,col,rowspan,colspan), ...]" (empty list if
  none), then output the HTML immediately after on the next line(s).
- Convert tables to HTML format, wrapped in <table>...</table>, matching
  the structure line exactly.
```

> 注意:变体 D 会在输出里混入非 Markdown 的"TABLE STRUCTURE: ...”行,评测前需要用一个轻量后处理正则把这类行剥离(见 §5 代码改动),否则会污染 Text Edit 指标——这是先污染再清洗,不是先污染再假装没看见。

### 变体 E(few-shot,验证 H4)

在官方 prompt 最前面插入一个纯文字描述的合并单元格示例(不带图片,避免过度增加 token/延迟):

```
Example: a table where the top-left cell spans 2 rows and 2 columns
(a merged header cell labeled "Category") must be rendered as:
<table><tr><th rowspan="2" colspan="2">Category</th><th>A</th><th>B</th></tr>
<tr><th>C</th><th>D</th></tr>...</table>
-- not as four separate cells each repeating "Category".
```

### 变体 F(公式规范,验证 H5)

替换官方 prompt 第 2 节为:

```
2. Mathematical Formula Processing:
- Convert all mathematical formulas to LaTeX format; never use Unicode
  math symbols (e.g. ×, ÷, ≤, √) inside a formula -- always use the LaTeX
  command (\times, \div, \leq, \sqrt{}).
- Enclose inline formulas with \( \). Enclose block formulas with \[ \].
- For piecewise/cases definitions use \begin{cases} ... \end{cases}.
- For multi-line aligned derivations use \begin{aligned} ... \end{aligned}.
- For matrices use \begin{array}{...} or \begin{pmatrix} as appropriate,
  matching the visual bracket style ([ ], ( ), or none).
```

### 组合变体(最终候选)

- **B+C**(表格结构化 + 多栏阅读顺序):预期收益最大,风险最低(两处改动方向不冲突)。
- **B+C+F**:在 B+C 基础上叠加公式规范化。
- 变体 D、E 作为**对照/消融实验**,不直接进入最终候选,只用来验证 H2/H4 是否值得投入。

---

## 4. 实验设计(如何在不烧穿预算的前提下验证)

### 4.1 不跑全量 1651 页做每一个变体

全量跑一次约 1651 页 × 若干秒/页(§8 实测过程耗时较长,且端点无并发优化、还在两个后端间随机切换),6 个变体全跑一遍全量代价太高。改用**分层抽样子集**:

- 从 OmniDocBench 的 `page_info.attribute` 标签里,取以下几个和短板直接相关的子集各 30 页(有放回按 `data_source`/`subset` 分层,不足 30 页的类别全取):
  - `subset: table_hard`
  - `subset: layout_hard`
  - `data_source: newspaper`
  - `data_source: academic_literature`
  - `layout: double_column` / `layout: three_column` / `layout: 1andmore_column`(合并抽样)
  - `subset: equation_hard`(留给变体 F)
- 再加一个 **50 页的随机对照子集**(不筛类别),防止"优化了表格子集但拖累了简单页面"这种此消彼长被漏掉。
- 子集总量约 200–250 页,与本仓库 opendataloader-bench 那 200 篇的评测预算量级相当,几十分钟内可以跑完一个变体。

### 4.2 控制混淆因素:两个后端混合作答

§8 已确认 `127.0.0.1:8087` 在 `Qwen3.5-4B` 和 `Qwen3.8-27B`(NBSP 前缀 id)之间随机切换。如果不控制这个变量,prompt A 和 prompt B 的分数差异可能只是"恰好某个变体多被 27B backend 回答了几次"的噪声,而不是 prompt 本身的效果。

**处理办法**(需要代码改动,见 §5):
1. 生成阶段把响应体里的 `"model"` 字段(真实回答该页的后端 id)记录进一个 sidecar 文件 `omnidoc_pred/<name>/_backend_map.json`(`{stem: model_id}`)。
2. 评测汇总阶段,除了看整体子集分数,**再按 backend 拆开算一次**——即"只看 27B 回答的那些页"和"只看 4B 回答的那些页"分别的分数变化。如果两个 backend 上改进方向一致,才认为是 prompt 的真实效果;如果只在一个 backend 上有效,要在报告里明确写出来,不能笼统合并成一个数。

### 4.3 指标与判定标准

- 主指标:子集内 Table TEDS↑(当前最大短板)。
- 护栏指标(不能因为优化表格而变差太多):Text Edit↓、Reading Order Edit↓。
- 判定一个变体"值得推广到全量"的门槛:
  - Table TEDS 相对基线子集分数**提升 ≥ 0.03**(约 4%+ 相对提升,不是噪声量级——基线全量跑的 table 类别间波动本身就有 0.02–0.03 的自然抖动,门槛设在噪声之上)。
  - 同时 Text Edit / Reading Order Edit **恶化不超过 0.01**(绝对值)。
  - 在两个 backend 拆分后**方向一致**(见 4.2),不是单后端的偶然效应。
- 若多个变体都过门槛,选 Table TEDS 提升最大且 Formula Edit 未恶化的一个进入全量复核。

### 4.4 执行顺序

1. 用当前基线 prompt,**先在同一个 200–250 页子集上重新测一次基线分数**(不能直接拿全量 1651 页的基线分数和子集分数比,子集分布本身偏向困难样本,数值天然会比全量差,必须有一个"子集基线"作为公平对照)。
2. 依次跑变体 B、C、B+C、F、B+C+F(优先级从高到低,前一个不达标也继续跑后一个,不要因为一个变体失败就假设方向错了)。
3. D、E 作为消融实验,时间允许再跑(验证 H2/H4,不是主线)。
4. 选出通过 §4.3 门槛、且综合最优的变体,**在全量 1651 页上跑一次最终确认**,只跑这一个变体的全量,不是每个变体都跑全量。
5. 把子集实验的完整对比表 + 全量确认结果一起写回 `BENCHMARK_REPORT.md`,包括**未通过门槛的变体**(反面结果同样要记录,不能只报喜)。

---

## 5. 代码改动清单

都在 `benchmark/` 目录下做,不动 `uparser/` 核心代码(这次是纯 prompt/评测脚本层面的实验,不涉及 uparser CLI 本身)。

1. **`benchmark/gen_qwen_omnidoc.py`**
   - 把硬编码的 `PROMPT` 常量拆成 `PROMPTS: dict[str, str]`(`baseline`/`variant_b`/`variant_c`/`variant_bc`/`variant_d`/`variant_e`/`variant_f`/`variant_bcf`),新增 `--prompt-variant` CLI 参数,默认 `baseline`(即当前 §8 用的官方参考 prompt,保证不传参数时行为不变)。
   - `call_once` 里响应解析成功后,把 `resp.json()["model"]` 写入 `out_dir.parent / f"{args.name}_backend_map.json"`(追加/合并写,注意并发写入需要加锁,复用现有 `ThreadPoolExecutor` 场景下已有的写文件模式)。
   - 变体 D 专用:新增一个 `strip_structure_line(text: str) -> str` 后处理函数,用正则去掉形如 `^TABLE STRUCTURE: .*$` 的行,写文件前调用;其余变体不受影响。
2. **`benchmark/select_omnidoc_subset.py`(新增)**:按 §4.1 的分层规则从 `OmniDocBench.json` 里选出子集,输出一个 `OmniDocBenchData/omnidoc_subset_tablehard.json`(与原始 ground truth 同结构,官方评测器可以直接当 `ground_truth.data_path` 用,不用改评测器代码)。
3. **`benchmark/summarize_omnidoc_by_backend.py`(新增)**:读 `_backend_map.json` + 官方评测器输出的 per-page edit 文件(`*_quick_match_table_per_page_edit.json` 等已经是逐页分数),按 backend id 分组重新聚合,实现 §4.2 的拆分对照。
4. `benchmark/gen_qwen_omnidoc.py` 的 `write_config`/`evaluate` 改为接受 `--dataset` 覆盖(该参数已存在,子集实验直接传 `--dataset OmniDocBenchData/omnidoc_subset_tablehard.json` 即可,**不需要新增**,这里只是确认现有参数够用)。

---

## 6. 风险与已知限制

- **端点不稳定是最大外部风险**:如果两个 backend 的模型能力差异本身就很大(4B vs 27B),即使 prompt 改进有效,也可能被"这次子集恰好多是 4B 回答"的抽样噪声掩盖——这是 §4.2 拆分对照要专门处理的问题,不能假装它不存在。
- **子集分数不能线性外推到全量**:子集是刻意偏向困难类别抽样的,子集上的提升幅度大概率会比全量上的提升幅度大(简单页面本来就接近满分,没有提升空间)——最终结论以 §4.4 步骤 4 的全量复核为准,子集实验只用来筛选候选、控制成本。
- **没有 CDM 环境**:Formula 相关的判定始终只能用 Edit_dist 代理,§8 已有的局限在这里同样适用,不重复展开。
- **variant D 的中间结构描述行**存在"以为后处理干净了、实际上格式变体导致正则没匹配上从而残留污染"的风险,必须在跑子集实验前先用等基线阶段同款的方法(先小样本跑 6–12 页人工检查输出,再跑子集)——即复用 §8.2 遇到过的"先小规模验证清洗逻辑生效,再放大规模跑"的做法,不能这次图省事跳过。

---

## 7. 交付物

1. 本文件(执行方案,已完成)。
2. 子集实验对比表(所有变体,含未通过门槛的)→ 追加进 `BENCHMARK_REPORT.md` 新的一节。
3. 通过门槛的最优变体在全量 1651 页上的最终分数 → 同样追加进 `BENCHMARK_REPORT.md`,并更新 §8.4 表格,标注"已被 prompt 改进版本取代"或"作为并列基线保留"(取决于是否所有指标都不劣于基线)。
4. 代码:`gen_qwen_omnidoc.py` 的 prompt 变体支持 + 两个新脚本,提交前跑一遍 `--limit 12` 冒烟测试(复用 §8 已验证的模式)。
