# mineru-vlm 推理/后处理链优化方案 —— 模型不变，冲击并超越官方 OmniDocBench 榜单

> 日期:2026-08-13 · 目标读者:在 `uparser/crates/uparser-core` 上迭代的工程 · 语言:实现全部在 Rust 侧,参照实现为 Python(`mineru_vl_utils`)
> 前置事实来自本轮 OmniDocBench 全量 1651 页实测(见 `BENCHMARK_REPORT.md` 第二部分)+ 对官方 MinerU 链、我方链、OmniDocBench 打分器三方源码的逐行对读 + 对逐样本评测产物的误差归因。

> **⚠️ 执行进度与结论修正见文末「附录 A:实测执行记录」** —— 已在 200 页 dev 子集上完成 F1 / F3 / F4 的严格 A/B,并对 F2 做了关键 gating 检查。结论有重要修正:**低风险快改基本不动主指标(Text Edit),F2 的模型 hint 不可用**。请先读附录 A 再看下面的原始规划。

---

## 0. 目标与现状差距(同一模型)

**约束:模型权重不变**(本机 endpoint 服务 `MinerU2.5-Pro-2604-1.2B`),只优化「图像预处理 + 请求参数 + 输出解析 + 后处理 + Markdown 渲染」这条链。

| 指标(OmniDocBench 口径) | 我方 mineru-vlm 实测 | 官方榜 MinerU2.5-Pro | 差距 |
|---|---|---|---|
| Text Edit↓ | **0.068** | 0.036 | **2× 差**(最大头) |
| Formula Edit↓ | 0.097 | (CDM,不可直接比) | — |
| Table TEDS↑ / TEDS-S↑ | **0.900 / 0.930** | 0.934 / 0.959 | -0.034 / -0.029 |
| Reading Order Edit↓ | **0.134** | 0.120 | -0.014 |

**总方针**:同模型下,文本/阅读顺序的「天花板 ≈ 官方自家 pipeline 的质量」。因此路线是 **两步**:
1. **对齐 v1.0.5**(下节):把参照实现从 v0.1.14 升级到官方 pinned 的 **v1.0.5**,补齐其推理/后处理链 → 逼近榜单(≈0.036 / 0.934 / 0.120)。
2. **在其之上做增量超越**:表格 HTML 保真、可选的 LLM 标题分级(本机有 Qwen 通用端点)、几何阅读顺序 tie-break、更强去重 —— 在官方 pipeline 相对薄弱处取分,实现「超越」。

---

## 1. 关键使能项:参照实现从 v0.1.14 → v1.0.5(本机已存在)

我方 `adapters/mineru_vlm.rs` 是对 **`mineru_vl_utils` v0.1.14** 逆向的(见文件头注释)。但官方 MinerU pinned 的是 **`mineru-vl-utils>=1.0.5,<2`**,且本机就有:

- 权威版 v1.0.5:`~/anaconda3/envs/minerUEnv/lib/python3.12/site-packages/mineru_vl_utils/`
- 旧版 v0.1.14:`~/anaconda3/envs/vllmlatestEnv/lib/python3.12/site-packages/mineru_vl_utils/`
- 官方后端编排:`opensource/MinerU/mineru/backend/vlm/`

**v1.0.5 相对 v0.1.14 的净增量(即我方缺失的部分)**:多框 `re.DOTALL` 版面正则 + 旋转 token + `merge_prev`(`txt_contd_tgt`)续段提示、表内块过滤、`image/chart` 的 "Image Analysis" 提示与内容再分类、表格图像遮罩、跨页表格合并、以及一整套 block 级后处理(公式/文本 fixer、`list_item→text`、`[Non-Text]` 清理、`equation_block` 合并)。

> **这是最高杠杆的一步**:先照 v1.0.5 源码把下面 F1–F9 逐条对齐,大部分差距会自然收敛。不要再对 v0.1.14 逆向。

---

## 2. 误差归因(为什么差距在这里,证据)

打分器语义(`OmniDocBench/src/core/preprocess/data_preprocess.py`、`match_quick.py`):
- 文本编辑距离在 `normalized_text` 上算,**大小写/所有空白/所有标点/`#`/`-`/`*`/连字符换行 全部被抹掉** → 只有真实字符(字母/数字/CJK)算数;**全角 vs 半角 字母数字算数(不做 NFKC)**。
- **未匹配的 GT block = 满分 1.0 惩罚**(召回);多余的 pred 文本 block 基本无害(被丢弃);段落被拆分是**被容忍的**(matcher 会把连续 pred 行合并回一条 GT 行,但**依赖 pred 的阅读顺序**)。

逐样本归因(基于 `result/mineru-vlm_quick_match_*_result.json`):
- 标题很好(mean edit **0.030**);正文 `text_block`(11669 条)mean edit **0.131** 是主要成本。
- **367 条 text_block 被漏(pred 空)** → 每条贡献 ~1.0(召回)。
- 出错文本 block 的编辑字符里 **73.5% 是「段落切分主导」**(一整段连续缺失);其中 **62.7% 的缺失文本其实出现在我方输出的别处**(纯匹配/顺序问题,可后处理修复),37.3% 是真缺失。
- **44.9% 的文本编辑字符量落在含公式的 block**(行内/嵌入数学);而独立 display 公式本身很好(漏检 0.1%,mean edit 0.081)。
- 阅读顺序:**46% 的页面不完美**(mean 0.134)。

**结论**:差距集中在 ①正文段落切分/召回、②行内与嵌入公式的分段、③表格结构保真、④二段识别的解码质量。逐条对应下面的工作项。

---

## 3. 工作项(按 ROI 排序)

每项标注:**做什么 / 为什么(打哪个指标)/ 改哪里 / 预期 / 风险**。文件路径均在 `uparser/crates/uparser-core/src/`。

### Tier 1 —— 高收益、低风险(请求参数 + 后处理,不改模型)

#### F1. 二段(stage-2)采样对齐为贪心解码 ★最先做
- **做什么**:`stage2_prompt_and_sampling`(`adapters/mineru_vlm.rs:115-142`)当前**只**设 `presence_penalty`/`frequency_penalty`/`skip_special_tokens`,漏了 `temperature`/`top_p`/`top_k`/`no_repeat_ngram_size` → 二段识别用的是**服务端默认采样(很可能非贪心)**。官方二段继承 base 默认 `temperature=0.0, top_p=0.01, top_k=1, no_repeat_ngram_size=100` + 每类 penalty(`mineru_client.py:34-76`)。补齐这四个键到全部三个 stage-2 分支(table/equation/default)。
- **为什么**:二段是**真正读出所有文本/表格/公式内容**的地方;非贪心解码直接制造 OCR 级字符错误 —— 对应误差归因里「ocr-like」占出错 block 的 **58%**。**打 Text/Formula Edit + Table TEDS(cell 文本)**。
- **附带 F1b**:官方把 `no_repeat_ngram_size` 放在请求体的 **`vllm_xargs`** 下(`http_client.py:279-283`),我方放在顶层(`mineru_vlm.rs:104`)。确认本机 vLLM 是否认顶层键;不认就包一层 `vllm_xargs`,一/二段都改。
- **改哪里**:`adapters/mineru_vlm.rs`(sampling 常量)、必要时 `transport.rs`(请求体拼装)。
- **预期**:中大幅降低 Text/Formula Edit;**几乎零风险**(纯采样对齐官方)。**先单独 A/B 这一项**量化收益。

#### F2. 用 `merge_prev`(`txt_contd_tgt`)+ 几何做正文段落合并 ★最大文本杠杆
- **做什么**:
  1. 版面正则升级到 v1.0.5 的**多框 `re.DOTALL`** 版并**捕获 box 后的 tail**(`_layout_re`,`mineru_client.py:26-31`),从 tail 解析 `txt_contd_tgt` → 给 text block 置 `merge_prev=True`(`_parse_merge_prev`,`mineru_client.py:127-128`),写入我方 IR 的 `merge_hint`(`types.rs`)。
  2. 端口官方 `merge_para_text_blocks`(`opensource/MinerU/mineru/backend/vlm/para_block_utils.py:47-225`):按 `merge_prev` 提示 + 几何门(行高对齐、首字符非数字/大写、上一行不以句末标点结尾、宽度相近、bbox 交叠)合并连续 `text` block;barrier 为 `{title,interline_equation,list}`。
- **为什么**:这是让我方 block 边界与 GT 标注粒度一致的**核心机制**,直接修复「62.7% 缺失文本其实在别处」的段切分主导误差,并顺带改善阅读顺序(matcher 的连续合并依赖正确顺序)。**打 Text Edit + Reading Order**。
- **改哪里**:`output_parse.rs`(正则 + tail/merge_prev 解析)、`postprocess.rs`(把当前 8px/20px 的纯几何合并升级为 hint+几何,或新增 `merge_para_by_hint`)、`types.rs`(填充 `merge_hint`)、`adapters/mineru_vlm.rs`(写 hint)。
- **预期**:最大的 Text Edit 收益来源。**风险中**:合并逻辑有行为漂移风险,需用 dev 子集回归;`postprocess.rs` 为所有协议共享,注意别改坏 native/opendataloader 口径(见 §5 护栏)。

#### F3. 识别列表内容(把 `list`/`list_item` 移出 SKIP_CONTENT,识别后归一到 text)
- **做什么**:`SKIP_CONTENT`(`adapters/mineru_vlm.rs:40`)当前含 `"list"` → 列表 block **完全不调模型、渲染成空**。官方对 `list_item` 用 "Text Recognition" 识别,再在后处理里 **`list_item → text`**(`post_process/__init__.py:190-191`)。改为:不 skip `list`/`list_item`,用 default(Text Recognition)提示识别,内容并入正文文本流。
- **为什么**:纯召回损失 —— 漏掉的列表正文每条 ~1.0。**打 Text Edit(召回)+ Reading Order**。
- **改哪里**:`adapters/mineru_vlm.rs`(SKIP_CONTENT、类别归一)、`category_map.rs`(`list`/`listitem` 现映射到 `"list"`;评估改映射到 `"text"` 以对齐官方,或在渲染层把 list 当普通段落)、`render/mod.rs`(注意当前 `list → "- "` 分支对 mineru-vlm 不可达)。
- **预期**:中等召回收益。**风险低**。

#### F4. 识别嵌入公式块(把 `equation_block` 移出 SKIP,补 `do_handle_equation_block`)
- **做什么**:`equation_block` 现在也在 SKIP_CONTENT → 被跳过、内容为空,对应「37.3% 真缺失」里的 `\begin{array}`/多行公式。官方对 `equation_block` **识别 + `do_handle_equation_block` 合并多行 + `\[ \]` 包裹**(`post_process/__init__.py:213-218`)。改为识别 `equation_block`(Formula Recognition 提示),多行合并后作为 display 公式产出。
- **为什么**:找回嵌入的多行/数组公式。**打 Text/Formula Edit(召回)**。
- **改哪里**:`adapters/mineru_vlm.rs`(SKIP_CONTENT、stage2 提示按 equation)、`formula_repair.rs`(多行合并/包裹)。
- **预期**:中等(公式密集页,含 `equation_hard` 子集)。**风险低**。

#### F5. 页眉/页脚/页码/侧栏/脚注 从正文 Markdown 丢弃(discarded)
- **做什么**:官方把 `header/footer/page_number/aside_text/page_footnote` 归入 **discarded_blocks,不进 Markdown**(`vlm_magic_model.py:265-272`)。我方把它们当普通段落渲染(`render/mod.rs`,category `_` 分支)。改为:这些类别不进入主 Markdown 正文流(可选保留到 content-list)。
- **为什么**:打分器虽会把多余 pred 文本丢弃(不直接加 Text Edit),但它们**改变阅读顺序位置、可能挤占正确匹配**;官方丢弃更干净。**打 Reading Order + 稳住 Text 匹配**。
- **改哪里**:`adapters/mineru_vlm.rs` 或 `render/mod.rs`(按 normalized category 过滤)、`types.rs`(可加 discarded 标记)。
- **预期**:小-中(主要 Reading Order)。**风险低**。

### Tier 2 —— 中收益

#### F6. 行内 vs display 公式保真(端口 `try_convert_display_to_inline`)
- **做什么**:我方 equation 类 block 一律 `$$…$$`/`\[…\]`(全 display);官方在后处理有 **`try_convert_display_to_inline`** 把本应行内的公式转回 `$…$`(`post_process/__init__.py:205-211`),渲染层行内 `$…$`、display `$$…$$`(`vlm_middle_json_mkcontent.py:20-30`)。同时保证**二段文本识别输出里的行内 `$…$` 被原样保留**(不被误当 display、不被吞)。
- **为什么**:44.9% 文本编辑量与公式相关;行内/display 混淆既加 Text Edit(行内公式没被打分器识别成公式),也加 Formula Edit。**打 Text/Formula Edit**。
- **改哪里**:`formula_repair.rs`(新增 display→inline 判定)、`render/mod.rs`、`output_parse.rs`(保留行内定界符)。
- **预期**:中。**风险中**(判定规则需对齐官方)。

#### F7. 表格 OTSL→HTML 保真(端口 v1.0.5 `otsl2html.py`)
- **做什么**:`otsl.rs` 自 P1 未变、是「尽力而为」实现。端口 v1.0.5 `otsl2html.py`:**ragged-row `<ecel>` 补齐**(`:120-157`)、`replace_table_image_tokens`、`replace_table_formula_delimiters`(官方 `enable_table_formula_eq_wrap=True`)。产出干净 `<tr><td>`(打分器会剥掉 `th/thead/span/div`,故只需结构 + `colspan/rowspan` 正确)。
- **为什么**:TEDS-S 0.930 vs 0.959 说明是**结构**错(缺行/跨行跨列)。**打 Table TEDS/TEDS-S**,也是**超越官方**最现实的抓手之一。
- **改哪里**:`otsl.rs`(共享,注意其它协议也用)。
- **预期**:中(TEDS 直接受益)。**风险中**(共享模块,需回归其它协议表格测试)。

#### F8. 表内块过滤(丢弃 ≥0.9 落在表格 bbox 内的 text/equation 框)
- **做什么**:官方 `_filter_table_internal_layout_blocks`(`mineru_client.py:271-280`,阈值 0.9)把落在 table 内的 text/equation/equation_block 丢掉,避免单元格文本又作为独立文本块重复产出。我方只有 IoU 0.8 通用去重,覆盖不到「包含关系」。
- **为什么**:去掉重复/串味的正文块(减少多余 pred 与错配)。**打 Text Edit + Reading Order**。
- **改哪里**:`geometry.rs`(新增 cover-ratio 判定)、`adapters/mineru_vlm.rs`(dedupe 后追加过滤)。
- **预期**:小-中。**风险低**。

#### F9. 全角→半角(仅字母/数字),并移除我方半角→全角标点转换
- **做什么**:官方 `full_to_half_exclude_marks`:把全角 `A–Z/a–z/0–9`(`FF21–FF3A/FF41–FF5A/FF10–FF19`)转 ASCII,**标点不转**。我方 `content_normalize.rs` 相反(CJK 文本半角标点→全角)且不转全角数字。因打分器抹标点(转标点是无用功)、但**保留字母数字的全/半角差异**,应改为:对齐官方做全角字母数字→半角,并去掉半角→全角标点转换。
- **为什么**:字符级免费收益(全角数字若与 GT 半角不一致会计成错)。**打 Text Edit**。
- **改哪里**:`content_normalize.rs`(共享,注意 native/其它协议)。
- **预期**:小但免费。**风险低**(但共享模块需回归)。

### Tier 3 —— 低收益 / 需谨慎(对齐或增量超越)

- **F10. 多框 DOTALL 版面正则**(v1.0.5):跨行 box 内容 + 旋转 token + tail。F2 已需要,合并做。
- **F11. image/chart 内容再分类**(`process_image_or_chart` → pure_table/pure_formula/chart/image,`post_process/__init__.py:124-170`):找回被误标成图的表/公式。频次低。
- **F12. 公式/文本 fixer 全量端口**(7 个 equation fixer + `try_fix_macro_spacing_in_markdown` + `try_move_underscores_outside`):边际 Formula Edit。
- **F13. 光栅化 filter 对齐**:一段 1036² resize 官方用 **BICUBIC**,我方 `hard_resize` 用 CatmullRom(`imaging.rs`);裁剪 `resize_by_need` 官方 v1.0.5 用 `math.ceil`、v0.1.14 用 `round`。边际,统一到 v1.0.5。
- **超越增量(官方默认没开的)**:
  - **LLM 标题分级**:官方 `apply_title_leveling` 默认关(所有标题都是单 `#`)。OmniDocBench 的 heading 指标层级无关,但**阅读顺序/结构**可受益于更准的标题识别;本机有 Qwen 通用端点(`localhost:8087` 或 `19121`),可选做一个轻量 LLM 标题分级/正文-标题纠偏。
  - **几何阅读顺序 tie-break**:官方阅读顺序 = 模型输出顺序,无几何兜底;我方已有 `reading_order.rs` XY-cut,可在模型顺序**明显不合理**时做几何校正(仅作为 tie-break,避免破坏正常页)。

---

## 4. 实施相位与顺序

| 相位 | 内容 | 依赖 | 主要指标 |
|---|---|---|---|
| P-A | **F1**(二段贪心采样)单独上,A/B 量化 | 无 | Text/Formula Edit、TEDS |
| P-B | **F2**(merge_prev 段合并)+ F10(正则) | v1.0.5 源码 | Text Edit、Reading Order |
| P-C | **F3 + F4 + F5**(列表/公式块召回、页眉页脚丢弃) | F2 | Text Edit(召回)、Reading Order |
| P-D | **F7 + F8**(表格保真、表内过滤) | 无 | Table TEDS、Text Edit |
| P-E | **F6 + F9**(行内公式、全半角) | F2 | Text/Formula Edit |
| P-F | Tier 3 + 超越增量(F11/F12/F13、LLM 标题、几何 RO) | 前面全绿 | 边际 + 超越 |

每相位跑一次评测、留分,累加对比。**F1 先行**是因为它零风险且可能一次拿到最大单项收益,能校准后续预期。

---

## 5. 验证协议与护栏

**迭代评测**(复用本轮已跑通的流程):
- 生成预测:`NO_PROXY=127.0.0.1,localhost uparser parse --protocol mineru-vlm --endpoint http://127.0.0.1:19122/v1/chat/completions --model MinerU2.5-2604-1.2B --format markdown --no-assets --no-cache <img>` → `<stem>.md`。
- 打分:`benchmark/OmniDocBench` 内 `.venv/bin/python run_eval.py --config configs/omnidoc_mineru-vlm.yaml`(**必须用 `run_eval.py` 的 spawn 包装**,否则 TEDS 因 fork-from-thread 全被打成 0)。
- **快迭代用固定 200 页 dev 子集**(从 1651 里分层抽样:含 `equation_hard`/`table_hard`/多栏/中英文各若干),定稿再跑全量 1651,避免每次 ~15 分钟。
- 每项改动**单独 A/B**(改一项、跑一次),避免收益/回归相互掩盖 —— 本项目历史上多次「正确代码没接进真实路径」正是这类耦合掩盖所致。

**护栏(共享模块回归)**:
- `render/mod.rs`、`postprocess.rs`、`otsl.rs`、`content_normalize.rs`、`geometry.rs` 被**所有协议共享**。任何改动必须:
  1. `cargo test --workspace` 全绿(含 render 的 insta 快照、mineru-vs-monkey 一致性合同测试);
  2. **重跑第一部分 opendataloader-bench**(native + mineru-vlm),确认 native 的 0.875 不回退、mineru-vlm 的 0.928 不回退;
  3. `clippy --all-targets` / `fmt` 干净。
- native 路径**不走**该渲染器(其 markdown 走内嵌引擎),F1–F13 基本不影响 native;但 `postprocess`/`content_normalize`/`otsl` 若被 native 间接触及需确认。

**「超越官方」的诚实口径**:
- 本机 endpoint 是 `MinerU2.5-Pro-2604`,榜单是 `2605`,有小版本差;声称「超越」时应控制或注明该差异(理想是拿到 2605 权重复测,或只在同一 checkpoint 下对比我方 vs 官方 pipeline)。
- 本轮关闭了 CDM(无 TeX);公式若要与榜单 CDM 口径比,需补 TeX/Ghostscript 工具链后开启 CDM。
- 目标分层表述:**先达到与官方 pipeline 同 checkpoint 的 parity(≈0.036/0.934/0.120),再靠 F7/F8 表格保真 + 超越增量在 TEDS/Reading Order 上取得净正 delta** —— 这是同模型下「超越」最现实的落点。

---

## 6. 一页速查:差距 → 修复

| 观测(证据) | 根因(官方 vs 我方) | 修复 | 主指标 |
|---|---|---|---|
| 58% 出错块是 ocr-like 字符错 | 二段用服务端默认采样(非贪心) | **F1** 二段贪心对齐 | Text/Formula/TEDS |
| 73.5% 编辑量是段切分主导,62.7% 文本在别处 | 无 `merge_prev` 段合并 | **F2** merge_prev+几何合并 | Text、RO |
| 367 条 text 漏 + 列表内容全空 | `list` 在 SKIP_CONTENT | **F3** 识别列表→text | Text 召回 |
| 37.3% 真缺失多为多行/数组公式 | `equation_block` 在 SKIP_CONTENT | **F4** 识别 equation_block | Text/Formula |
| 页眉页脚当正文 | 官方 discarded,我方渲染成段落 | **F5** 丢弃 discarded | RO |
| 45% 文本编辑量与公式相关 | 全 display,无行内 | **F6** display→inline | Text/Formula |
| TEDS-S 0.930<0.959(结构错) | OTSL→HTML 尽力而为 | **F7** 端口 v1.0.5 otsl2html | TEDS |
| 表内文本重复成独立块 | 无表内过滤 | **F8** cover≥0.9 过滤 | Text、RO |
| 全/半角字母数字算数 | 我方转标点方向相反、不转数字 | **F9** 全角字母数字→半角 | Text |

---

# 附录 A:实测执行记录(2026-08-13)

对 P-A 起的项目做了严格 A/B:先在 200 页分层 dev 子集快迭代,**关键项再跑全量 1651 复核**(每项单独改、单独跑、`--no-cache`、spawn 版 `run_eval.py`)。**结论:低/中风险各项均无法净正撬动文本主指标;最终只保留 F1(保真型 no-op),F3/F4/N1 全部回退。**

## A.1 dev200 A/B(Δ 相对 baseline)

| 变更 | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order↓ |
|---|---|---|---|---|---|
| baseline | 0.0701 | 0.1107 | 0.9450 | 0.9782 | 0.1225 |
| **F1**(二段贪心采样) | +0.0001 | −0.0027 | +0.0008 | +0.0000 | +0.0004 |
| F3+F4(识别 list+equation_block) | +0.0021 | −0.0030 | +0.0007 | +0.0000 | +0.0043 |
| F1+F4(仅识别 equation_block) | +0.0001 | −0.0030 | +0.0067 | +0.0046 | +0.0033 |
| F1+F4+**N1**(几何段落合并) | **+0.0056** | −0.0031 | +0.0007 | +0.0000 | **+0.0077** |

## A.2 ⚠️ dev200 有偏,必须全量复核 —— F4 在全量上其实是负的

dev200 是**分层抽样**(为覆盖难例,纳入了全部 `equation_hard`/`table_hard`),导致**公式/表格页被显著高配**(200 页里 54 表格 / 38 公式)。这让 F4(识别 equation_block)在 dev200 上看着净正(公式/表格 ↑、文本持平)。**但在全量 1651 的自然分布上复核,结论反转**:

| 全量 1651 | Text Edit↓ | Formula Edit↓ | Table TEDS↑ | Table TEDS-S↑ | Reading Order↓ |
|---|---|---|---|---|---|
| baseline | 0.0678 | 0.0971 | 0.9004 | 0.9295 | 0.1335 |
| F1+F4 | **0.0706 (+0.0028)** | 0.0945 (−0.0026) | 0.9017 (+0.0013) | 0.9309 (+0.0014) | **0.1378 (+0.0043)** |

F4 找回的公式块在自然分布上**挤占文本匹配、扰乱阅读顺序**,text/RO 的回退盖过了公式的小收益 → **对文本加权的 headline 是净负**,故 F4 **回退**。**教训:分层 dev 子集只能筛掉明显坏项,任何看起来正向的改动必须在全量自然分布上复核后才能采纳。**

## A.3 各项处置(最终)

- **F1(二段贪心采样)—— 保留。** 新增 `base_greedy_sampling()`,一/二段共享 `temperature=0, top_p=0.01, top_k=1, no_repeat_ngram_size=100`;修掉「二段漏设贪心参数、走服务端默认采样」的保真缺陷。dev200 上基本 no-op(text +0.0001),**不改质量、只更贴官方、更确定**,保留。(原以为二段非贪心造成 58% 的 ocr-like 错误 —— A/B 证伪,本 endpoint 二段本就近似贪心。)
- **F3(识别 list 容器)—— 回退。** 重复 item 文本、挤占匹配、乱序,dev200 text/RO 双退。
- **F4(识别 equation_block)—— 回退。** 见 A.2,全量上 text/RO 净负。
- **N1(几何段落合并)—— 回退。** 见 A.4。

## A.4 关键实验:F2/N1 的段落合并路线走不通 ⛔

1. **F2 的模型 hint 不可用**:直接打一次原始 `Layout Detection` dump 生输出 —— 模型**每框都发 `<|rotate_up|>`**,但**发 0 个 `txt_contd_tgt`(续段 hint)**。官方靠 `merge_prev` hint 做段合并这条路在本 checkpoint 上不成立;生输出也证实模型输出**行级小框**(一页 14 个 text 小框),即我方相对 GT 过切分。
2. **纯几何段落合并(N1)实测回退**:实现了 adapter 内的几何 line→paragraph 合并(同列 x 交叠 + 行距 ≈ 行高 + 句末标点断段守卫),dev200 上 **Text Edit +0.0056、Reading Order +0.0077 双退**。根因是打分器的**非对称性**:它自带 `deal_with_truncated` 会**重组过切分的连续 pred 行**(容忍 under-merge),但**over-merge**(把两个 GT 段错并一段)会让被吞的 GT 段失配、计 edit=1.0。所以几何合并**只会亏**——打分器自家的重组比我们的几何合并更准。已回退。

## A.5 修正后的总结论

- **同模型下,文本主指标(0.068 vs 官方 0.036)不可能靠「采样对齐 / 召回补齐 / 几何段合并」这类低-中风险改动撬动。** 四项(F1/F3/F4/N1)实测:要么 no-op,要么净负。打分器对段落切分的容忍 + over-merge 惩罚,使召回/合并类改动天然难赢。
- **真正逼近 0.036 需要「v1.0.5 全套后处理链的完整移植」**(含官方 `merge_para_text_blocks` 的**全部**几何门 + 宽度/首字符规则 + 文本 fixer),**且大概率还需同一 2605 checkpoint**(本机 2604)。这是大工程,单项收益仍不保证 —— 属高投入、中等不确定。
- **唯一现实、正期望的「超越」抓手是表格 TEDS**(F7 端口 v1.0.5 `otsl2html`:ragged-row 补齐 + 结构保真)——它不依赖文本匹配、不受 over-merge 惩罚、且 dev200 已见表格方向为正。建议若继续投入,**只做 F7**,并在全量上复核。
- 已落地:**只有 F1**(`adapters/mineru_vlm.rs`,全测试 307 lib +29+2+755+2 绿、clippy/fmt 干净,`postprocess.rs` 已复原)。F3/F4/N1 的负结论已写入代码注释,防止后人重蹈。

## A.5 复现实验命令

```bash
# dev 子集(200 页)
python3 - # 见 OmniDocBenchData/OmniDocBench_dev200.json 的生成(分层抽样,seed=42)
# 生成预测(新二进制)
BIN=uparser/target/release/uparser
NO_PROXY=127.0.0.1,localhost $BIN parse --protocol mineru-vlm \
  --endpoint http://127.0.0.1:19122/v1/chat/completions --model MinerU2.5-2604-1.2B \
  --format markdown --no-assets --no-cache <img> > <stem>.md
# 打分(必须用 spawn 版 run_eval.py)
cd benchmark/OmniDocBench && .venv/bin/python run_eval.py --config configs/omnidoc_dev_f4.yaml
```
