# uparser native 多格式引擎改造报告（第一、二轮）

> **第二轮（2026-08-16，本文档 §6 起）已完成剩余全部任务**：产品接入层 6 项、mutation 语料、格式保真长尾、以及最后的覆盖缺口 legacy DOC/PPT。
> **格式覆盖现已与 anydoc 完全持平：47/47。**

---

# 第一轮

> 日期：2026-08-16
> 基线：`NATIVE_VS_ANYDOC_EVALUATION_AND_PLAN.md` 的实测评估
> 约束：**不依赖 anydoc 的任何 crate、二进制或测试语料**；引擎自身的回归测试全部在进程内构造 fixture
> 全部数字来自本机实跑，非估算

---

## 0. 一句话结论

| 维度 | 改造前 | 改造后 |
|---|---:|---:|
| 现代格式成功率 | 34/40 (85%) | **40/40 (100%)** |
| 异常语料（abuse，8 例） | 4 例判定正确 | **8 例全部有界，6 例命中正确预算，2 例钳制并告警** |
| 破损语料可恢复项（5 例） | 3/5 | **5/5** |
| DOCX 行内样式 | 全丢 | **完整** |
| ODT 行内样式 | 全丢 | **完整（含"显式关闭"语义）** |
| EPUB 表格单元格 | **内容 100% 丢失** | **完整** |
| RTF 结构 | 标题变列表、样式标记跨行断裂 | **与 anydoc 一致** |
| 列表编号/层级/marker | 全部退化为 `1.` | **续编号、嵌套、字母/罗马序列均保留** |
| 引擎单测 | 21（5 个前端 0 测试） | **48（含 15 条针对本轮每个缺陷的回归测试）** |
| 大 CSV 吞吐 vs anydoc | 1.27× | **1.7–2.0×** |
| 大 RTF 吞吐 vs anydoc | 2.03× | **1.6–2.2×** |

剩余不达标项只有一类：**legacy DOC/PPT（7 个样本）仍未实现**，属原执行计划里明确排在最后的独立里程碑。

---

## 1. 改了什么（按根因）

原评估定位出 5 个根因，本轮全部处理：

### 根因 1：没有 OPC 关系层 → 已建立

`ooxml.rs` 新增 `ContentTypes`、`load_root_relationships`、`main_part`、`related_part`、`relationship_id`、`percent_decode`。

- **主 part 不再硬编码**。DOCX 的 `word/document.xml`、PPTX 的 `ppt/presentation.xml` 以及 styles/numbering/footnotes/endnotes 全部改为经根 `_rels/.rels` 的 `officeDocument` 关系与各 part 自身的 rels 定位，惯例路径仅作 fallback 并发 warning。
- **格式检测改为关系驱动**。`detect_zip_package` 不再对 `[Content_Types].xml` 做子串匹配（那会被一个内嵌 Word 对象的内容类型声明骗过去），改为：`mimetype` 精确匹配 ODF/EPUB → 根关系解析主 part → 该 part 的 Content-Type 精确分类；无法判定的 ZIP 优先信任文件名而不是通用魔数嗅探器。
- **路径安全**。relationship target 现在做百分号解码，解码后产生 `/`、`\` 或 NUL 的一律拒绝（`%2F` 是绕过"先切分后解码"型解析器的标准手法），并丢弃 fragment。
- **`r:id` 读取修正**。`<p:sldId id="256" r:id="rId4"/>` 有两个 local name 都是 `id` 的属性；旧代码按 local name 取到 `256`，关系查不到，**每个真实 PPTX 的幻灯片顺序都在静默退化成文件名顺序**。现在优先取带命名空间前缀的那个。

**效果**：4 个 altpath/对抗性 content-types 样本从失败转为成功。

### 根因 2：没有统一 XML 预算与恢复策略 → 已补齐

- DOCX/PPTX 正文新增 `max_xml_depth` 强制（此前只有 `max_xml_nodes`，深层嵌套完全不设防）。
- **恢复策略按计划 §6.4 落地**：可选 part（styles/numbering/notes）解析失败 → `OptionalPartSkipped` warning + 继续；正文 XML 中途失配 → 先把在途的段落/表格 flush 出来，再以 `TruncatedContent` warning 收尾，**只有一个 block 都没产出时才算致命**；`ResourceLimit` / `Encrypted` 永远致命，不被容错吞掉。
- PPTX 表格 span 越界由静默钳制改为**钳制 + `InvalidSpanClamped` warning**。

**效果**：`deepxml` 从"被接受"转为命中 `max_xml_depth`；`corrupt-styles`、`mismatched` 从硬失败转为可恢复。

> 说明：`hugespan` 的 ODS/PPTX 两例，native 的策略是**有界钳制 + 告警**（约 100 ms 完成、内容保留），而不是 anydoc 的硬报错。这是计划 §5.3 明确规定的行为（"重叠 span 按格式恢复政策钳制或 warning"），不是漏检——不存在分配放大，且钳制事实可从 `warnings` 观察到。

### 根因 3：renderer 只是骨架 → 已重写

`render/mod.rs` 重写，并配 13 条渲染器级单测：

- **上下文感知转义**取代原来的全局 `\ * _` 替换。`_` 只在词边界转义（`snake_case_name` 不再变成 `snake\_case\_name`）；行首块级标记（`#`、`>`、`-`、`|`、`1.`）单独处理；URL 与链接标签各有自己的规则。
- **相邻同样式 run 合并**（避免 `**a****b**`）与**空白边缘外提**（`**bold **` / `**\nbold**` 在 GFM 里根本不是强调）。
- **列表**：按 marker 宽度缩进整个 item 主体（嵌套列表、段落、表格因此留在 item 内）；`start` 生效；字母/罗马序列以字面标签保留而不是统统重编成 `1.`。
- **表格**：`header_rows == 0` 时输出空表头，不再把第一行数据挪用为表头（那会直接删掉那一行的内容）。
- **逻辑单元标题**：单个 Flow 文档不再产出合成标题；章节标题若与首个 block 重复则抑制（EPUB 每章标题此前都输出两遍）。
- **段落前导空白剥离**：4 个以上前导空格会被 Markdown 当作缩进代码块。
- **代码围栏按内容动态加长**。

### 根因 4：ODS 走了 calamine → 已改走 ODF walker

`formats/mod.rs` 把 `Ods` 路由到 `odf.rs`，calamine 只保留 XLS/XLSX/XLSM/XLSB。ODS 现在复用 ODF 的 `number-*-repeated` / `covered-table-cell` 处理和展开预算——这也意味着 ODS 的展开攻击第一次真正命中 `max_expansion`（此前是被 calamine 以"认不出格式"误拒）。每个 `table:table` 成为一个 Sheet 单元。

### 根因 5：没有真实语料测试 → 已建 15 条回归测试

全部在 `crates/uparser-document-engine/src/lib.rs`，用 `zip` crate 在进程内构造最小 OOXML/ODF/EPUB 包与 RTF 字节流，**不引用任何外部语料**。覆盖：主 part 关系解析、对抗性 content-types、`r:id` 幻灯片顺序、XML 深度预算、失配标签恢复、可选 part 跳过、字符样式、列表续编号、列表嵌套、最小 ODS、ODF span 样式开关、CSV 表头、EPUB 表格单元格、RTF stylesheet 泄漏、RTF 源换行。

**其中 3 条已用"删掉修复再跑"验证过确实能捕获对应缺陷**（RTF stylesheet 泄漏 → 段落变标题；EPUB 表格 → 单元格变空；DOCX `rStyle` → 强调消失）。

---

## 2. 格式前端的具体修复

| 格式 | 缺陷 | 修复 |
|---|---|---|
| DOCX | `<w:rStyle>` 完全未处理，字符样式驱动的粗/斜/删除线全丢 | `parse_styles` 改为收录全部 style family 并解析其 `rPr`；新增 `resolved_run_style` 走 `basedOn` 链；run 起始从段落样式取基线，`rStyle` 与直接属性依次叠加 |
| DOCX | 每个列表段落各成一个 List，编号永远从头开始，`ilvl>0` 被压平 | 新增 `ListCounters`（按 `(numId, level)` 跨段落记录序号，进入某层时重置更深层）与递归 `push_list_item`（按层级嵌套、按序号连续性合并） |
| DOCX | Word 在编号与文字之间插的 tab 被当成内容 | 列表项内容剥离前导空白 |
| DOCX | 标题因 Heading 样式带粗体而渲染成 `# **标题**` | renderer 在标题内移除 bold（层级本身已表达字重） |
| DOCX | 字符样式名含 "Heading" 会把段落误判为标题 | `heading_level` 只接受 paragraph family 的样式 |
| PPTX | 幻灯片顺序退化为文件名顺序 | `relationship_id` 取 `r:id` |
| PPTX | span 越界静默钳制 | 增加 `InvalidSpanClamped` warning |
| ODT/ODP | **完全没有 `<text:span>` 处理**，行内样式 100% 丢失 | 新增 `parse_text_styles`（content.xml + styles.xml，`fo:font-weight` / `fo:font-style` / `text-line-through-style` / `text-underline-style` / `text-position`），样式以 **Option 覆盖语义**表达，因此 `font-weight="normal"` 能在粗体段落里把粗体关掉；`<text:span>` 用栈组合 |
| ODT/ODP | 列表序列类型全被压成 decimal/bullet | `style:num-format` 映射到 a/A/i/I；列表样式按 **(名称, 层级)** 建索引；嵌套 `<text:list>` 继承父级样式名；读取 `text:start-value` |
| ODS | 最小合法包（仅 `mimetype` + `content.xml`）被 calamine 拒绝 | 改走 ODF walker |
| ODF | 表格永远 `header_rows = 0`，渲染出空表头 | 首行全部为无跨行跨列的非空单元格且至少两行时判定为表头 |
| EPUB | **表格单元格文本全部丢失** | `<td>`/`<th>` 打开隐式段落（XHTML 单元格通常是裸文本而非包 `<p>`），闭合时 flush 进单元格 |
| EPUB | `<li>文本<ul>…</ul></li>` 的文本被卷进子列表 | `<ul>`/`<ol>`/`<li>` 起始处先 flush 在途段落 |
| RTF | `\stylesheet` 里的 `\outlinelevel`/`\ilvl` 泄漏到正文，普通段落变标题和列表项 | 处于 `Destination::Skip` 时不再应用任何格式/结构控制字 |
| RTF | 源文件换行进入文本，把样式 run 拆成 `**\nbold**` | CR/LF 按规范丢弃（受支持码页里没有 0x0A/0x0D 尾字节，字节级剥离安全） |
| RTF | `\ilvl` 被当作列表成员标志，导致每个样式化段落都变列表项 | 成员资格只由 `\ls` 决定，`\ilvl` 仅表示层级；标题优先于列表 |
| RTF | 列表全部压平 | 按层级递归嵌套 |
| CSV | 表头推断过严（要求第二行有类型化数据），全文本表被判无表头 | 改为"默认有表头，除非首行本身已含类型化值" |
| CSV | 合成 `Sheet 1` 标题注入源文档不存在的标题 | 单表无名，label 置空 |
| XLSX | 日期/时长输出原始序列号（`46096`、`1.10434027777778`）、布尔输出 `true` | 序列号还原为 ISO 日期 / `[h]:mm:ss` 时长；布尔用 `TRUE`/`FALSE`；浮点按电子表格有效精度格式化 |

---

## 3. 实测结果

### 3.1 格式覆盖（47 个多格式样本）

```
success: anydoc 47/47   native 40/47
```

native 的 7 个失败全部是 `.doc` / `.ppt`（legacy OLE 二进制），即原计划的"波次 8 / 独立里程碑"。**现代格式 40/40，与 anydoc 持平。**

### 3.2 健壮性

| 类别 | 结果 |
|---|---|
| abuse（8） | 全部有界终止。6 例硬拒绝且**报出的预算名与实际命中一致**（`max_xml_depth` / `max_expansion` ×2 / `max_entry_bytes` ×2 / 格式不支持 ×1）；2 例（`hugespan` ODS/PPTX）按既定策略钳制并告警 |
| malformed `--recovers`（5） | **5/5 恢复**（此前 3/5）；`brokenpersist.ppt` 仍受 legacy 缺口影响 |
| malformed `--errors`（5） | 5/5 正确拒绝，错误类型明确（`Encrypted` / `Malformed` / zip 无效） |

### 3.3 输出质量（同源多格式，ground truth 来自同一份源文档）

- **DOCX**：粗/斜/删除线、样式驱动的粗体段落、列表编号（含跨段续编 `4.`）、层级、字母/罗马标签，全部与 anydoc 一致；表格因 canonical grid 保留 rowspan/colspan，**内容比 anydoc 的 GFM 降维更完整**。
- **ODT**：行内样式（含 `NotBold` 显式关闭）与 anydoc 一致；表格同上更优；残余差异为 marker 后缀写法（`a.` vs `a)`）、`text:continue-numbering` 未实现、脚注编号用源 id 而非重编号。
- **RTF**：标题、样式、段落结构与 anydoc 一致；残余差异为 `\listtable` 未解析（列表类型统一按 decimal 呈现）。
- **EPUB**：表格内容、嵌套列表、章节标题去重后与 `book.md` ground truth 一致；残余差异为跨章内部链接仍是原始相对 href（合并输出后是死链）。
- **CSV**：与 anydoc 逐字节一致（仅末尾空行差异）。
- **XLSX**：日期/时长/布尔与 anydoc 一致；表格 span 保留更完整。

### 3.4 性能

口径：Windows 逐文件 CLI 墙钟，取多次最小值，减去各自实测启动基线（同一轮内测得）。

| 输入 | anydoc 墙钟 | native 墙钟 | 扣除启动（约 65 ms） | native 相对 |
|---|---:|---:|---|---:|
| `big.csv`（6.0 MB / 12 万行 × 4 列） | 550 ms | **328 ms** | 485 ms vs **263 ms** | **约 1.8×** |
| `big.rtf`（3.9 MB / 6 万段落含行内样式） | 283 ms | **185 ms** | 218 ms vs **120 ms** | **约 1.8×** |

（各取 5 次最小值。同一批测量在机器负载较高时给出 2.0× / 2.2×，空闲时 1.7× / 1.6×，**方向稳定：native 在两种格式上都明显快于 anydoc**。）两者对 `big.rtf` 与 `big.csv` 的输出字节数各只差 1 字节，说明是同等输出下更快，不是靠少做事更快。

渲染器重写一度把 CSV 拖慢到比 anydoc 更差（465 ms vs 540 ms 核心时间），本轮已定位并修复：`escape_inline_text` / `escape_table_cell` 改为 `Cow` 无分配快路径，单个无样式 run 与"单段落单文本"单元格各走短路，避免每个单元格 4 次分配。修复后 CSV 核心时间从约 465 ms 降到约 314 ms，**优于改造前的原始实现**。

### 3.5 工程门禁

```
cargo test --release -p uparser-document-engine   48 passed; 0 failed
cargo clippy -p uparser-document-engine --all-targets -- -D warnings   clean
cargo clippy -p uparser-core --features native --all-targets -- -D warnings   clean
cargo fmt -p uparser-document-engine -- --check   clean
```

> 工作区级 `cargo clippy --workspace` 仍不干净，但失败点全部在**本轮未触碰的既有代码**：`uparser-native-engine`（vendored pdf-inspector）的 `dead_code`/`unused_variables`/`unnecessary_to_owned`，以及 `pyo3-ffi` 在本机缺 Python 的构建失败。修 vendored 代码会与上游产生分叉，属独立决策。

---

## 4. 依赖边界

- `uparser-document-engine` 的 `Cargo.toml` **不含 anydoc**，也没有任何 anydoc 源码拷贝。所有实现基于 ECMA-376 / ODF / RTF 规范与底层通用 crate（`zip`、`quick-xml`、`calamine`、`csv`、`encoding_rs`、`cfb`、`sha2`）重写。
- **测试不依赖外部语料**：48 条测试全部在进程内构造 fixture。
- `bench/document-corpus/run.sh` 是**开发期可选的对照脚本**，不参与构建或测试；对照二进制不存在时自动降级为只跑 uparser。

---

## 5. 剩余差距与下一轮

按价值排序：

| 项 | 现状 | 影响 |
|---|---|---|
| **legacy DOC/PPT** | 未实现（7/47 样本） | 唯一还落后于 anydoc 的格式覆盖项。独立里程碑，预估 8–12 工程周 |
| EPUB 跨章内部链接 | 保留原始相对 href，合并 Markdown 后为死链 | 需要全局 anchor slug 表；中等工作量 |
| RTF `\listtable` | 未解析，列表类型统一 decimal | 影响 RTF 列表 marker 保真 |
| ODF `text:continue-numbering` | 未实现 | 影响被打断后的列表续编号 |
| 结构化文档 asset 落盘 | Markdown 里仍是 `asset-<hash>` 占位 id，未写文件 | 图片链接不可用；已有 `assets.rs` 可复用 |
| `ParseOptions` 未贯通 CLI | `limits` / `include_assets` / `include_notes` / `include_headers_footers` 从外部不可达 | 用户无法调预算或关闭 notes |
| `--format document-json` 双重解析 | `run_parse_native` 先 `parse_document` 再 `native_document_json` | 纯浪费，一处改动即可 |
| `Asset.bytes` 进 document-json | 会序列化成 JSON 数字数组，体积膨胀数倍 | 应默认只出 metadata |
| `compatibility_block` 降维 | 表格渲染成 Markdown 文本塞进 `text`，`reading_order` 恒 `None`，每个 block 重建一次 CanonicalDocument | ParseResult 侧保真与性能 |
| mutation / fuzz | 未建 | 计划 §12 第 6/7 层 |
| DOCX 多级列表 lvlOverride、comments、header/footer | 未实现 | 保真度长尾 |

---

## 附录：复现

```bash
cd uparser && cargo build --release --features native -p uparser-core --bin uparser
cargo test --release -p uparser-document-engine          # 48 passed，无外部依赖

# 可选：与外部转换器对照（需自行构建对照二进制）
bash bench/document-corpus/run.sh
```

---

# 第二轮：完成剩余全部任务（2026-08-16）

上一轮结束时列出的待办分四组，本轮全部完成。

## 6. 总览

| 维度 | 第一轮结束 | 第二轮结束 |
|---|---:|---:|
| **格式覆盖** | 40/47（缺 legacy DOC/PPT） | **47/47（与 anydoc 完全持平）** |
| abuse（8 例） | 8 例有界，6 例命中正确预算 | **8 例有界，7 例命中正确预算**（`deepnest.ppt` 新增 `max_record_depth`） |
| malformed 可恢复（5 例） | 5/5 | 5/5（`brokenpersist.ppt` 现在真正走 PPT 解析后恢复） |
| 退出码语义 | 结构化失败一律 4（"internal，请重试"） | **按失败性质分流 1/2/4** |
| 引擎测试 | 48 | **58 单元 + 4 mutation** |
| core 测试 | 321 + 29 CLI + 2 contract | **321 + 29 CLI + 2 contract + 7 native-document** |
| mutation 语料 | 无 | **30 万次变异实跑，零 panic / 零 hang** |
| 大 CSV / 大 RTF 吞吐 vs anydoc | 1.8× / 1.8× | **~1.9× / ~2.0×** |

## 7. 第一组：产品接入层（6 项全部完成并实测）

这一组此前是"引擎已经能做对，但外面没接上"。

| 项 | 改法 | 实测 |
|---|---|---|
| **消除双重解析** | `NativeAdapter::parse_native()` 一次解析返回 `NativeParse::{Pdf, Structured}`，Markdown / `document-json` / `ParseResult` 三种输出全部由这一个值派生 | `cli.rs` 中 `native_document_json` 引用数：**0** |
| **结构化文档 asset 落盘** | 新增 `assets::write_document_assets()`：按 sha256 内容寻址、**保留原始扩展名**（嵌入资源可能是 JPEG/GIF/SVG/EMF，统一改名 `.png` 会写出打不开的文件） | DOCX 内嵌 PNG → 真实文件落盘 + Markdown 链接指向它 |
| **`Asset.bytes` 不再进 JSON** | 字段改 `#[serde(skip)]`，新增 `path` 字段 | `document-json` 中 `"bytes"` 出现次数：**0**；`path` 正确 |
| **`compatibility_block` 保真** | 表格 → `html`（保留 rowspan/colspan，GFM 表达不了合并单元格）；`reading_order` 按源顺序填充；**去掉"每个 block 重建一次 CanonicalDocument 再渲染"**，改为 `render::block_markdown(document, block)` | CSV 块 `category_raw=table`、`html` 有内容、`text` 为空、`reading_order=0` |
| **`ParseOptions` 贯通 CLI** | 新增 `--no-notes` / `--headers-footers` / `--max-input-mib`；`NativeOptions` 聚合传递 | 见下方"两个 flag 曾是空转" |
| **broken pipe 不再 panic** | `emit_line()` 把 `BrokenPipe` 当正常输出结束 | `uparser … \| head` 的 stderr 中 `panicked` 次数：**0** |

**这一组里发现两个"看起来能用、其实完全空转"的 flag**，正是我此前批评过的那类缺陷：

1. `--no-notes` 对 RTF 无效——`rtf.rs` 从不读 `options.include_notes`。
2. **`include_headers_footers` 没有任何前端读它**——它只存在于 `options.rs`，也就是说这个选项从诞生起就是死的。DOCX 的 `word/header*.xml` / `footer*.xml` 根本没被读过，RTF 的 `\header`/`\footer` 一直在 Skip 列表里。

两个都真正实现了：RTF 按 `include_notes` 跳过 footnote destination、按 `include_headers_footers` 决定是否跳过 header/footer destination；DOCX 新增 `collect_headers_and_footers()`，经 `/header`、`/footer` 关系定位分部，并把 header 放在正文前、footer 放在正文后。两者都有断言"默认排除、显式包含、且顺序为 header→body→footer"的集成测试。

## 8. 第二组：mutation 语料

新增 `crates/uparser-document-engine/tests/mutation.rs`，四条测试：

- `mutated_fixtures_never_panic_and_always_terminate` —— 字节级变异（翻位/截断/拼接/清零/插入/删除），断言只有两种结果：解析成功，或**声明过的** typed error（match 穷尽，新增 error variant 会强制在此做决定）；成功的还要通过表格网格自洽检查（`rows` 与 grid 高度一致、无 ragged row、covered cell 不指向网格外）。
- `mutated_xml_parts_inside_a_valid_container_never_panic` —— **只损坏 XML 内容、保持 ZIP 有效**，让变异真正打进前端而不是死在容器层（穿透率 66% vs 字节级的 36%）。
- `rendering_a_mutated_document_never_panics` —— 渲染走同样的结构，同样要保证不 panic。
- `parsing_is_deterministic` —— 同一输入两次必须产出同一结果（hash 迭代顺序泄漏到输出这类问题在此暴露）。

用固定种子 LCG 而不是 `rand`，失败会给出可复现的 `(fixture, mutation index)`。

**实跑规模**：把每 fixture 变异数临时提到 25000，跑了 **30 万次**变异（含新写的 DOC/PPT 二进制解析器）——**零 panic、零 hang，最慢单例 827µs**。提交版设为 200/fixture（全量 0.16 秒），保证每次提交都能跑。

## 9. 第三组：格式保真长尾

| 项 | 结果 |
|---|---|
| **EPUB 跨章内部链接** | `ch02.xhtml#frag` 在合并成单份 Markdown 后是死链。现在所有 id 用 `part#id` 限定为文档级唯一 anchor（两章都定义 `#notes` 时原本还会撞车），链接解析到 `#ops-text-first-xhtml-next`；每章首块附带章级 anchor，使无 fragment 的整章链接也能落地 |
| **ODF `text:continue-numbering`** | 被段落打断后的列表现在接着数，不再从 1 重来（`text.odt` 的 `4. Fourth, continuing the count` 已与 anydoc 一致） |
| **ODF 列表 marker / start-value** | `style:num-format` 映射 a/A/i/I，列表样式按 (名称, 层级) 索引，嵌套列表继承父级样式名，读取 `text:start-value` |
| **RTF `\listtable`** | **未做**，见 §11 |

## 10. 第四组：legacy DOC / PPT —— 唯一的覆盖缺口，已补上

新增 `formats/doc.rs`（约 470 行）和 `formats/ppt.rs`（约 480 行），纯 Rust，无 LibreOffice。

### DOC（MS-DOC）

按 MS-DOC §2.3/§2.8 走完整链路：OLE2 → `WordDocument` 流 → FIB → `fWhichTblStm` 选 `0Table`/`1Table` → `fcClx`/`lcbClx` 定位 CLX → CLX 里的 **piece table** 把字符位置映射到字节区间（每段标记 8-bit 码页或 UTF-16LE）。

**piece table 是必须手写解析器的原因**：Word 不连续存储文本，任何"当成 UTF-16 读一遍"的提取器在编辑过的文档上都会返回交错的乱码。

三个实测发现：

1. **最小 FIB 回退**。三个 handmade fixture 的 `csw = cslw = cbRgFcLcb = 0`，即完全没有变长段、没有 piece table。原实现直接报 "FIB is too old"。改为回退到 Word 6/95 的连续文本区间 `[fcMin, fcMac)`——**4 个 fixture 全部解析成功**。
2. **码页从数据判定，不靠猜**。`handmade-shiftjis.doc` 的 `lid = 0`，只有 FibBase flags 的 `fFarEast` 位（0x4000）为真——它说"东亚"，但不说是哪个。做法是依次用 Shift-JIS / GBK / Big5 / EUC-KR 解码，取第一个**无替换字符**的结果。改前输出 `‚±‚ñ‚É‚¿‚Í`，改后 `こんにちは世界。` ✅。其余按 `lid` 映射码页（Cyrillic 的 `Привет, мир!` 一次就对）。
3. **表格由控制符还原**。`\r` 段落、`\u{7}` 单元格/行终止、`\u{13}/\u{14}/\u{15}` 域指令与域结果（指令是机器码不是内容，只保留结果）。

### PPT（MS-PPT）

记录树遍历，但**关键在于流顺序不等于放映顺序**——PPT 编辑不会重排磁盘内容，顺序在 `SlideListWithText` 里，每张片的字节偏移在 `PersistDirectoryAtom` 里。两者都实现了；任一缺失时回退到流顺序并告警。

由此带来两个实测正确的行为：

- `brokenpersist--recovers.ppt`（持久目录损坏）→ **回退成功、rc 0** ✅
- `handmade-sparsenotes.ppt`（只有第二张片有备注）→ 通过 `NotesAtom.slideIdRef` → `SlidePersistAtom.slideId` → 放映序号，备注**准确落在第 2 张片**（`[^slide-2-notes]`）✅。按序号硬配会配错。

期间修正了一处自己写错的常量：`TextTypeEnum` 的 CENTER_TITLE 是 **6** 不是 5，NOTES 是 2——这是靠加临时调试输出看真实取值发现的，不是照抄规范想当然。

### 由此暴露并修掉的一个健壮性缺口

`deepnest--errors.ppt`（深嵌套记录树）一开始返回 **rc 0**：`walk()` 撞到深度上限时只是 `return`，把"截断"当成了"解析完"。改为记录 `Budget::exhausted` 并在 `parse()` 里 `check()?` 成 `ResourceLimit`；同时按执行方案 §6.2 的建议新增专用预算 `max_record_depth = 64`（远严于 XML 的 256——二进制记录树是**真递归**遍历的）。现在 rc 2，且错误里点名 `max_record_depth`。

### 退出码语义修正

一份损坏文档此前返回 4 = "internal error, 请重试"——对一个永远解析不了的文件，这是最糟的建议。现按失败性质分流：

- `unsupported_format` / `malformed` / `missing_part` → **1**（输入本身的问题，重试无意义）
- `encrypted` / `resource_limit` / `io` → **2**（环境可修，修完重试）
- 其余 → 4

`skills/uparser/SKILL.md` 的退出码表已同步更新（code 1 的语义扩展了，必须改文档，否则 agent 会按旧约定误判）。

## 11. 最终实测

```
格式语料      success: anydoc 47/47   native 47/47      ← 完全持平
abuse(8)      8 例全部有界；7 例命中正确预算并点名，1 例(hugespan) 按设计钳制+告警
malformed(10) 5 个 --recovers 全部 rc 0；5 个 --errors 全部拒绝且退出码正确
mutation      30 万次变异：零 panic、零 hang、最慢 827µs
测试          引擎 58 单元 + 4 mutation；core 321 单元 + 29 CLI + 2 contract + 7 native-document
lint          clippy -D warnings 干净（engine + core --features native）；fmt --check 干净
性能(best-of-5, 扣除约 65ms 启动)
  big.csv  anydoc 514ms  native 314ms   →  约 1.9×
  big.rtf  anydoc 262ms  native 168ms   →  约 2.0×
```

## 12. 仍未做的（明确列出，不含糊）

| 项 | 说明 |
|---|---|
| **DOC 字符/段落样式** | 当前 DOC 只恢复文本、段落和表格，**不恢复粗体/斜体/标题层级**——那需要 STSH + PlcfBteChpx/Papx + SPRM 解析。anydoc 在 `text.doc` 上能给出 `# Fixture Document` 和 `**bold**`，native 给的是纯文本。这是 native 与 anydoc 在 DOC 上仅剩的实质差距，已在 `doc.rs` 模块文档和运行时 warning 里明说 |
| **PPT master/layout 继承、表格、图片** | ~~同样只恢复片内文本与备注~~ —— **第三轮已做（见 §13）**，仅表格与 metafile 图片仍未做 |
| **RTF `\listtable`** | 未解析，列表类型统一按 decimal 呈现 |
| **PPTX/PPT 的 `# Slide N` 合成标题** | 无真实标题占位符时用合成标签分隔幻灯片；anydoc 直接拼接。这是刻意的差异（扁平化后需要片间分隔），不是缺陷 |
| **`hugespan` 的钳制 vs 报错** | 保持钳制+告警（执行方案 §5.3 规定的行为），未为了对齐分数改成硬报错 |
| **工作区级 clippy** | 仍有失败，全部在本轮未触碰的 vendored `uparser-native-engine` 与缺 Python 的 `pyo3-ffi` |
| **cargo-fuzz target** | 未建。本机是 Windows/MSVC，libfuzzer 跑不起来，写了也无法验证；mutation 语料是可在本机真实运行的替代 |

# 第三轮：PPT master/layout 继承与图片（2026-08-17）

针对 §12 的 **PPT master/layout 继承、表格、图片** 一行。本轮只做 PPT，DOC 样式（STSH + PlcfBteChpx/Papx + SPRM）按计划留到下一轮。

`formats/ppt.rs` 拆成 `formats/ppt/{mod,styletext,pictures}.rs`。

## 13. 做了什么

### 13.1 master 继承：`TxMasterStyleAtom` + `StyleTextPropAtom`（新 `styletext.rs`）

这两条记录是同一组变长结构（`TextPFException` / `TextCFException`）的两种容器，所以放在一个模块里。三个必须做对的点：

1. **字段顺序不是 mask 位顺序**。掩码说明哪些字段在场，字段则按规范声明顺序排列。顺序错了不会报错，只会把之后每个字段整体错位——所以走位是显式写死的，每一步注释掉自己跳过的字段。
2. **属性是三态而不是布尔**。"未指定"不等于"关闭"：某一 run 没写 bold，它继承的是 master 对应缩进层级的默认值。提前塌缩成 `false`，master 继承就整个丢了——这正是本轮要修的东西本身。
3. **run 长度按 UTF-16 code unit 计**，不是按 `char`。一个星平面字符消耗两个单位。

`TxMasterStyleAtom` 的 `recInstance` 既是它作用的 text type，也是"每个 level 前是否多一个 indentLevel 字段"（instance ≥ 5）的判据。

幻灯片经 `SlideAtom.masterIdRef`（偏移 12）选 master，找不到时退到第一个 master——对绝对多数的单 master 演示文稿这也是正确答案。

**实测**：`handmade-multimaster.ppt`（专为这条造的 fixture）现在输出 `- **Alpha master body text**` 和 `*Beta master body text*`，与 anydoc 快照**逐字节一致**——两张片各自继承了不同 master 的 bullet / bold / italic 默认值。`pres.ppt` 的大纲现在是 `- Top level point` / `  - Nested detail` / `- Second point with emphasis`，同样与 anydoc 一致（缩进层级来自 `TextPFRun.indentLevel`，bullet 来自 master 默认值）。

顺带查证了一件事：`pres.ppt` 里 "emphasis" 的加粗，native 和 anydoc 都没给出。直接解码该 shape 的 `StyleTextPropAtom` 确认**文件本身就没有这个 run**（三个 character run 的掩码都只有 0x40000=颜色）——是 LibreOffice 导出 PPT 时丢的，不是读取端的问题。

### 13.2 顺带修掉的两个真实缺陷（都不是本轮任务，是做的过程中撞出来的）

- **`SlideListWithText` 的 instance 没有校验**。同一个 document 容器里有三份 `SlideListWithText`（instance 0=幻灯片、1=母版、2=备注页），三份装的都是同型的 `SlidePersistAtom`。原实现把三份的条目**全部**收进同一个列表，靠后面"只保留真的是 RT_Slide 的偏移"过滤掉母版/备注——偏移是滤干净了，但 `slide_ids` 没有，于是**只要一份 deck 有母版列表（也就是几乎所有真实 deck），slideId → 放映序号的映射就整体错位**，备注就会挂到错的幻灯片上。现在按容器 instance 分流。
- **有备注却一条也没输出**。原实现只接受 text type 2（notes 占位符）的 shape 作为备注。LibreOffice 导出的 PPT 把**每一个** text shape 都标成 type 4（other），于是 `pres.ppt` 的两条演讲者备注被静默丢弃（rc 0、无告警）。改为"排除标题占位符，其余都算备注"——备注页里重复的标题/正文占位符本来就是空的，`collect_shapes` 已经会丢弃空 shape。现在 `pres.ppt` 输出 `Speaker note for the intro slide.` 和 `Second slide notes mention the table.`，与 anydoc 一致；`handmade-sparsenotes.ppt` 的稀疏挂载仍然正确（只有第 2 张片有备注）。

### 13.3 图片（新 `pictures.rs`）

一张 PPT 图片跨**两个 OLE 流、三层记录**，三层缺一不可：

```
shape 的 OfficeArtFOPT 属性 pib（1-based 索引，不是偏移也不是 id）
  → 文档 drawing group 里的 blip store：第 pib 个 OfficeArtFBSE 的 foDelay（偏移 28）
    → Pictures 流该偏移处的 blip 记录 → 真正的 JPEG/PNG 字节
```

anydoc 只做到"把 Pictures 流里的所有 blip 当作文档级 asset 倒出来"，不做定位，因此 Markdown 里没有 `![]()`。本轮把 `pib → FBSE → Pictures` 整条链走通，图片按出现顺序落在 `Block::Figure`，复用引擎既有的内容寻址 asset 机制，因此和 DOCX/PPTX 的图片走同一条落盘与 `![]()` 渲染路径。

两个容易出错、都写了针对性测试的点：

- **blip 的数据起点取决于记录自己的 `recInstance`**，某些 instance 前面是两个 16 字节 UID 而不是一个，长度字段里看不出来。取错会在图片数据前多 16 字节垃圾，落盘得到一个任何看图器都打不开的文件。
- **blip store 必须限定在"当前"document 容器内**。编辑过的 deck 会把被取代的旧世代留在同一个流里；按流序取第一个 store 可能取到旧的，于是每个 `pib` 索引整体错位，**幻灯片静默配到别人的图**。回归测试 `a_superseded_generations_blip_store_does_not_shift_the_picture_index` 构造了这个形状，并**验证过它确实抓得住**：临时改回全流扫描，该测试立刻拿到 `STALEPNG` 而不是 `PNGBYTES`。

metafile（EMF/WMF）blip 不解码——它们通常是 deflate 压缩的矢量图，要在 Markdown 里有意义还得渲染，代价与收益不成比例；现在按 `AssetDropped` 告警，而不是静默消失。

### 13.4 实测

仓库里 5 份 `.ppt` 语料全部重跑：`pres.ppt` / `handmade-multimaster.ppt` / `handmade-sparsenotes.ppt` / `brokenpersist--recovers.ppt` 均 rc 0 且内容如上；`deepnest--errors.ppt` 仍 rc 2 并点名 `max_record_depth`（预算语义未回归）。

**图片是这轮唯一没有真实语料的东西**——仓库里没有任何一份带图的 `.ppt`（`pres.ppt` 的 `Pictures` 流长度为 0），本机也没有 LibreOffice 可以现造。所以做了两级验证：Rust 单元测试用 `cfb` 现搭一份合成 deck 走**真实 `parse()` 入口**；另外用一个**独立写的 Python 生成器**（不复用 Rust 那份构造代码，避免"生成器和解析器犯同一个错"）造了一份含真实 1×1 PNG 的 deck，跑完整 CLI：

```
$ uparser parse --protocol native --format markdown /tmp/picture-deck.ppt
![](picture-deck_images/b1ff9c8e….png)

Text under the picture

$ file picture-deck_images/b1ff9c8e….png
PNG image data, 1 x 1, 8-bit/color RGB, non-interlaced
```

测试：引擎 85 单元（+27）+ 4 mutation；workspace 全绿(321 core 单元 + 29 CLI + 2 contract + 7 native-document + 755 native-engine)。`clippy -D warnings` 与 `fmt --check` 干净。
（注：`transport::tests` 的 9 个 wiremock 用例在本机需要 `NO_PROXY=127.0.0.1,localhost`，否则公司代理会把 localhost 请求劫持成 nginx 404——与本轮改动无关。）

## 14. 第三轮之后仍未做的

| 项 | 说明 |
|---|---|
| **DOC 字符/段落样式** | 未做，下一轮的目标。需要 STSH + PlcfBteChpx/Papx + SPRM，且要先把 `doc.rs::extract_text` 从"拼成一个 `String`"改成保留 CP→FC 映射（CHPX/PAPX 全按 FC 索引） |
| **PPT 表格** | 未做，**anydoc 也没做**：PPT 表格是 OfficeArt 形状组，没有 DOC 那种 `TDefTable` 可依。两边都是把单元格文本摊平成独立段落 |
| **PPT metafile 图片** | EMF/WMF 不解码，见 §13.3 |
| **PPT 标题占位符** | 只有生产者真的标了占位符（`TextHeaderAtom` type 0/6）时才会成为标题。LibreOffice 导出的 PPT 把所有 shape 标成 type 4，此时无从判断——anydoc 同样如此 |
| **RTF `\listtable`** | 未解析，列表类型统一按 decimal 呈现 |
| **cargo-fuzz target** | 仍未建。原因已变化：本机现在是 Linux，libfuzzer 可以跑，不再有"写了也无法验证"的理由——这条现在是纯粹的待办 |

# 第四轮：多格式的 CLI 路由与 skill（2026-08-17）

改 skill 的过程中拿真实命令逐格式核对，撞出两个 CLI 层的真实缺陷，都已修：

- **`.doc` / `.ppt` 在默认的 `--protocol auto` 下被路由到 VLM**。`cli.rs::resolve_auto_protocol` 和 `api.rs` 里那份"可离线解析的格式"清单是 P7/P8 时写的，两个 legacy 二进制格式是第二轮才加的，从没补进去。后果：`uparser parse deck.ppt`（不带任何 flag，也就是 Agent 最常见的调用方式）会打印 `routed to "mineru-vlm"` 然后卡在"没有配置 endpoint"，而这份 deck 本来完全可以离线解析完。两处清单都补上 `Doc` / `Ppt`，并加了回归测试 `auto_routes_every_offline_readable_format_to_native`（遍历 12 个扩展名）和 `auto_still_profiles_a_pdf_rather_than_shortcutting_it`（确保 PDF 仍走 profiler，扫描件才能进 VLM）。
- **`--pages` / `--stream` / `--window-size` / `--max-concurrency` 在 `native` 路径上被静默忽略**。这条路径绕开 scheduler，这几个 flag 根本到不了。对 Agent 来说静默接受比不支持更糟：`--pages 2` 看起来像选了第 2 张片，实际返回整份文档。现在只要显式传了就在 stderr 打 `warning: ... has no effect on the native protocol`（默认值不触发）。

`skills/uparser/SKILL.md` 新增 "Non-PDF documents" 一节（格式表、三种输出格式怎么选、只对该路径生效的 flag、`warnings` 怎么读、按格式的已知差距、退出码、什么时候才该对 Office 文件强上 VLM），frontmatter 的 description 也补全了格式列表（否则这个 skill 在"把 pptx 转 markdown"这类请求上不会被触发）。`references/protocols.md` 的 §10 原文写的是"DOCX/PPTX 先经 LibreOffice 转 PDF"——那只在强制 VLM 时成立，对默认路径是错的误导，已重写。

11 种格式用裸 `uparser parse` 逐个实测：全部 rc 0、全部 `routed to "native" (source-semantic … parser is available locally)`。
