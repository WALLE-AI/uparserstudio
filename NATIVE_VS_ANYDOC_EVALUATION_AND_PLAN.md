# uparser native 多格式引擎完成度评估、anydoc 实测对比与执行方案

> 评估日期：2026-08-16
> 评估对象：`uparser/crates/uparser-document-engine`（HEAD `dc99cc8`）、`uparser-core` native adapter
> 对照对象：`opensource/anydoc` 0.1.9（本地源码，MIT）
> 证据等级：本文所有结论均来自**本机实际编译与实跑**，不引用历史报告数字。

---

## 0. 结论摘要（先看这一节）

| 问题 | 结论 |
|---|---|
| 代码是否完整完成？ | **否。** 三个层面未完成：① HEAD **无法编译**；② 声称"已支持"的 6 类格式在真实语料上失败；③ 执行计划 §6/§10/§12 三个公共层（Package/Renderer/测试体系）基本未落地。 |
| native 效果是否超过 anydoc？ | **质量与健壮性明显落后，吞吐性能已经领先。** 40 个现代格式样本 anydoc 成功 40/40，native 成功 34/40；异常语料 native 有 2 例该拒未拒、2 例该恢复却硬失败。 |
| native 性能是否超过 anydoc？ | **是，且是当前唯一确定的优势。** 大 CSV 约快 **1.27×**，大 RTF 约快 **2.0×**（扣除进程启动后）。这个优势需要在补齐质量的过程中守住。 |
| 距离"全面超过 anydoc"还差多少？ | 按本文方案，**8 个波次 / 约 10–14 工程周**（不含 legacy DOC/PPT）。其中前 3 个波次（约 3–4 周）就能把成功率与健壮性拉平。 |

---

## 1. 复现口径

```
OS        Windows 11 Pro 26200，shell = Git Bash
rustc     1.97.1 (8bab26f4f 2026-07-14)
native    uparser/target/release/uparser.exe   (cargo build --release --features native -p uparser-core --bin uparser)
anydoc    opensource/anydoc/target/release/examples/convert.exe  (cargo build --release --example convert)
语料      opensource/anydoc/tests/fixtures/**  （47 个格式样本 + 8 个 abuse + 10 个 malformed）
命令      native: uparser.exe parse <f> --protocol native --format markdown --no-assets
          anydoc: convert.exe <f> -o <out.md>
```

> ⚠️ **本次评估对工作区做了一处最小修改**：`crates/uparser-document-engine/src/formats/epub.rs:27-30`，把三个 metadata 字段改为 `.clone()`。原因见 §2.1——不改这一行，整个 workspace 无法编译，也就无法做任何实测。这是唯一的代码改动，未提交。

---

## 2. 完成度评估

### 2.1 【阻断级】HEAD 不编译

```
error[E0382]: borrow of partially moved value: `publication`
  --> crates\uparser-document-engine\src\formats\epub.rs:31:72
30 |     document.metadata.language = publication.language;
   |                                  -------------------- value partially moved here
31 |     if let Some(navigation) = load_navigation(&mut package, &opf_part, &publication, options)? {
   |                                                                        ^^^^^^^^^^^^ borrowed here after partial move
```

`git status` 干净，说明该错误就在 HEAD 提交里。`NATIVE_MULTIFORMAT_IMPLEMENTATION_STATUS.md` §7 声称"317 个库测试通过"，但**当前提交连编译都过不了**——EPUB 导航（`load_navigation`）是在那份状态报告之后加入且未验证的。

含义不只是"少了一行 clone"：**这个仓库没有任何 CI 门禁在拦截不可编译的提交**。

### 2.2 修复后测试确实全绿——但绿得没有意义

```
cargo test --release -p uparser-document-engine
test result: ok. 21 passed; 0 failed
```

21 个测试全过，然而同一份二进制在真实语料上失败 13/47。原因是测试全部是**手工构造的最小 XML 片段**，且集中在 `lib.rs`：

| 文件 | LOC | `#[test]` 数 |
|---|---:|---:|
| `formats/epub.rs` | 1018 | **0** |
| `formats/docx.rs` | 898 | **0** |
| `formats/odf.rs` | 820 | **0** |
| `formats/rtf.rs` | 679 | **0** |
| `formats/pptx.rs` | 560 | **0** |
| `formats/sheet.rs` | 219 | **0** |
| `lib.rs`（集成测试） | 625 | 14 |

五个最复杂的格式前端**自身零单元测试**；`tests/fixtures/` 下只有一个 `sample.csv`；没有 snapshot、没有 malformed 语料、没有 abuse 语料、没有 mutation、没有 fuzz、没有跨格式契约测试。执行计划 §12 的九层测试金字塔实现了第 1 层的一部分。

**最典型的例子**：`parses_epub_metadata_spine_xhtml_assets_lists_and_tables` 断言了表格的 `(rows, columns, header_rows) = (3,2,1)` 和 `row_span`，**唯独没断言任何一个单元格的文本**——所以下面 §3.2 那个"EPUB 表格所有单元格内容全部丢失"的严重缺陷，被这个测试完美地放过了。

### 2.3 规模对比：约完成计划的 1/3

| | uparser-document-engine | anydoc |
|---|---:|---:|
| 源文件数 | 17 | 66 |
| 总行数 | 6,197 | 17,143 |
| Package/容器层 | 63 行（单文件） | 1,178 行（archive/xml/relationships/path/limits） |
| Markdown renderer | 310 行（单文件） | ~1,180 行（anchors/escape/inline/table/mod + tests） |
| 共享 shared 层 | 无（ooxml.rs 212 行） | 1,832 行（14 个模块） |
| legacy DOC/PPT | 无 | 2,622 行 |

### 2.4 对照执行计划逐条核查

| 计划条目 | 状态 | 证据 |
|---|---|---|
| §5 CanonicalDocument v1 | ✅ **完成度高** | `model.rs` 337 行，Heading/List/Table/Note/Asset/Inline/Style/CellSlot 齐备，canonical grid invariant 成立 |
| §5.3 canonical table grid | ✅ 完成 | Origin/Covered 双态，span 展开前计费（CSV/ODF/EPUB 路径） |
| §5.5 lowering 到 ParseResult | ⚠️ **降维严重** | `native.rs::compatibility_block` 把表格渲染成 Markdown 文本塞进 `text`，未走计划要求的 `html` + rowspan/colspan；`reading_order` 恒为 `None`；List 的 category 被降为 `"text"`；**每个 block 都重建一个 CanonicalDocument 再渲染一次**（O(n) 次全量渲染） |
| §6.1 统一 Package API | ❌ **未实现** | `package.rs` 只有 `open/names/read/read_required`；无 `relationships_for`、无 `resolve_target`、无 part 缓存去重、无 path 安全层。各格式前端各自拼路径字符串 |
| §6.2 ResourceLimits 全覆盖 | ⚠️ **部分且不均** | 9 个预算里，DOCX 只强制 `max_xml_nodes`，PPTX 同样只有 1 项；`max_xml_depth`/`max_asset_bytes`/`max_text_bytes` 在 docx/pptx **完全没有检查**（实测后果见 §3.3） |
| §6.3 路径安全 | ❌ **未实现** | 无百分号解码、无 dot-traversal 拒绝、无编码分隔符拒绝 |
| §6.4 恢复政策 | ❌ **规则反了** | 规则 2 要求"optional part 损坏 → warning + 继续"，实测 `corrupt-styles--skips.docx` / `mismatched--recovers.docx` 都是**硬失败**（见 §3.3） |
| §7 内容优先的格式检测 | ❌ **有确定性缺陷** | `detect_zip_package` 用 `[Content_Types].xml` **子串匹配**，且 `wordprocessingml` 优先级最高；不读根 `_rels/.rels`。三个 PPTX 因此被判成 DOCX（见 §3.1） |
| §8.2 DOCX 主 part 经 relationship 定位 | ❌ **硬编码** | `const DOCUMENT_PART = "word/document.xml"`，styles/numbering/footnotes/endnotes 全部硬编码路径 |
| §8.2 style cascade | ⚠️ **段落有、字符没有** | 全仓库无 `rStyle` 处理 → 字符样式驱动的粗/斜/删除线全丢（见 §3.2） |
| §8.3 PPTX 主 part | ❌ **硬编码** | `ppt/presentation.xml` + `ppt/slides/slideN.xml` 数字扫描回退 |
| §8.4 ODS 走 ODF table walker | ❌ **走了 calamine** | `formats/mod.rs:18` 把 `Ods` 路由到 `sheet::parse`；`odf.rs` 只处理 Odt/Odp。后果：最小合法 ODS 直接失败（见 §3.1） |
| §8.7 legacy DOC/PPT | ❌ 未实现（计划内，最后波次） | `unsupported document format: Doc/Ppt` |
| §9.3 ParseOptions（fidelity/fallback/include_*） | ❌ **未接通** | `native.rs` 三处全部硬编码 `ParseOptions::default()`；CLI 无任何对应 flag，`limits`/`include_assets`/`include_headers_footers` 从外部不可达 |
| §9.4 CanonicalDocument cache | ❌ 未实现 | native 分支整体绕过 cache；`--format document-json` 还会**解析两遍**（`run_parse_native` 先 `parse_document` 再 `native_document_json`） |
| §10 统一 GFM renderer | ⚠️ **骨架级** | 310 行；转义只做 `\ * _` 全局替换（无上下文感知、无行首转义、无 backtick/链接标签转义）；无相邻 style run 合并；无空白边缘处理（实测产生 `**\nbold**` 这种坏标记）；无 anchor slug/去重；code fence 固定 `~~~` |
| §11.3 document-json schema | ✅ 完成 | `schema_version: "uparser.document.v1"` 就位 |
| §12 测试战略（9 层） | ❌ **仅第 1 层部分** | 见 §2.2 |
| §13 uparser-document-bench | ❌ 未建立 | 无按格式分桶的 benchmark，无质量指标脚本 |
| §15 删除 structured_bypass | ⚠️ 半完成 | native feature 下绕开，非 native 构建仍是旧路径；`ingest.rs` 的实现未删 |

---

## 3. anydoc vs native 实测对比

### 3.1 覆盖与成功率（47 个格式样本）

| 格式 | 样本数 | anydoc 成功 | native 成功 | native 失败原因 |
|---|---:|---:|---:|---|
| csv | 4 | 4 | 4 | — |
| xls / xlsx | 3 | 3 | 3 | — |
| ods | 3 | 3 | **1** | 2 例 `malformed: Cannot detect file format`（calamine 拒绝仅含 `mimetype`+`content.xml` 的最小合法 ODS） |
| docx | 10 | 10 | **9** | `handmade-altpath.docx`：`missing required part: word/document.xml` |
| pptx | 6 | 6 | **3** | 3 例同样报 `missing required part: word/document.xml` ← **被误判为 DOCX** |
| odt / odp | 6 | 6 | 6 | — |
| epub | 3 | 3 | 3 | — |
| rtf | 5 | 5 | 5 | — |
| **现代格式小计** | **40** | **40 (100%)** | **34 (85%)** | |
| doc / ppt（legacy） | 7 | 7 | 0 | 计划内未实现 |
| **合计** | **47** | **47** | **34** | |

**PPTX 误判的根因已定位**：`handmade-order.pptx` / `handmade-strict.pptx` 的 `[Content_Types].xml` 里**故意**声明了 `/word/document.xml` 的 wordprocessingml 内容类型（这是 anydoc 专门设计来验证"必须走根 `_rels/.rels` 的 officeDocument 关系、而不是对内容类型做子串匹配"的对抗样本）。native 的 `detect_zip_package` 恰好第一条就 `contains("wordprocessingml.document")`，全部中招。

**altpath 类失败的根因**：主 part 放在非惯例路径（`deck/pres.xml`、`content/main.xml`），必须经根关系解析。native 硬编码路径，必然失败。

这两类是**同一个根因**：缺少 OPC 关系层（计划 §6.1 明确要求，未实现）。

### 3.2 输出质量（同源多格式对照）

anydoc 的 `text.docx` / `text.odt` / `text.rtf` 由同一份 `tests/fixture-src/text.fodt` 生成，`book.epub` 由 `tests/fixture-src/book.md` 生成——这是**真实 ground truth**，无需第三方裁判。

**(a) 行内样式：native 全丢**（`text.docx`，GT 含 bold/italic/strike）

```diff
- Plain paragraph with **bold**, *italic*, and ~~struck~~ runs.     ← anydoc
+ Plain paragraph with bold, italic, and struck runs.               ← native
- **Style-bold paragraph with a NotBold-styled span inside.**       ← anydoc
+ Style-bold paragraph with a NotBold-styled span inside.           ← native
```
根因：`docx.rs` 处理 `<w:b>/<w:i>/<w:strike>` 直接属性，但**全仓库无 `rStyle` 处理**，字符样式驱动的格式全部丢失。

**(b) 列表：编号、层级、marker 类型、续编号全丢**（同一文件）

```diff
  1. First numbered
- 2. Second numbered
-    - a) Alpha sub one          ← anydoc：字母 marker + 缩进
-         - i. Roman sub sub     ← 罗马 marker + 二级缩进
- 4. Fourth, continuing the count ← 跨段落续编号
- - IV. Roman starting at four
+ 1. Alpha sub one              ← native：全部退化成 "1."，层级压平
+ 1. Roman sub sub
+ 1. Fourth, continuing the count
```
根因：`render_list` 用 `list.start.unwrap_or(1) + index`，且每个段落被拆成独立 List，`ListMarker` 的 alpha/roman 变体在 renderer 里被统一成 decimal。

**(c) RTF：结构性错乱**（`text.rtf`，GT 与 docx/odt 同源）

```
native 输出：
1. **
  Fixture Document**          ← 标题变成列表项，且 ** 跨行断裂
Plain paragraph with **
bold**
, *
italic*
, and ~~
struck~~
 runs.                        ← 每个 style run 内含换行，标记全部失配
```
两个独立缺陷叠加：RTF 前端未按 style table 还原标题/列表边界；renderer 没有做 style run 的空白边缘处理（anydoc `render/markdown/inline.rs` 240 行专门解决这件事）。

**(d) EPUB：表格单元格文本 100% 丢失**（`book.epub`，GT = `book.md`）

```
GT / anydoc:                     native:
| Name  | Qty |                  |  |  |
| Bolts | 12  |                  | --- | --- |
| Nuts  | 30  |                  |  |  |
                                 |  |  |
```
表格形状对了（rows/cols/header_rows 都对，所以测试通过），**内容全空**。这是本次发现的最严重的静默内容丢失。

同一文件还有两个缺陷：
- **每个章节标题被输出两次**（`render::markdown` 对每个 unit 无条件打印 `# {label}`，与章节自身的 `<h1>` 叠加）→ 标题过检；
- 内部链接保留原始相对 href（`ch002.xhtml#chapter-two`），但锚点渲染成 `<a id="markpoint">`，合并成单份 Markdown 后**全部是死链**；anydoc 统一 slug 化（`#epub-text-ch002-xhtml-markpoint`）。

**(e) native 确实更强的一处**：表格 span 保真。`handmade-tables.docx`：

```
anydoc（GFM 降维，内容丢失）：      native（canonical grid → HTML，span 保留）：
| Head A | Head B | Head C |        <table>
| --- | --- | --- |                   <tr><th rowspan="2">Head A</th>...
|  |  |  |                            <tr><td>legacy start</td><td>legacy cont</td>...
| legacy start<br>legacy cont |  |  |
```
CanonicalDocument 的 grid invariant 是这套设计**真实兑现**的部分，应当保留并作为对 anydoc 的差异化卖点。

### 3.3 健壮性（abuse 8 + malformed 10）

| 语料 | 期望 | anydoc | native | 判定 |
|---|---|---|---|---|
| `deepxml--errors.docx` | 拒绝 | 拒绝 (rc 1) | **接受 (rc 0)** | ❌ 该拒未拒：DOCX 未检查 `max_xml_depth` |
| `hugespan--errors.pptx` | 拒绝 | 拒绝 (rc 1) | **接受 (rc 0)** | ❌ 该拒未拒：PPTX 未对 span 展开计费 |
| `zipbomb--errors.docx` | 拒绝 | 拒绝 | 拒绝（`max_entry_bytes`） | ✅ |
| `imagebomb--errors.docx` | 拒绝 | 拒绝 | 拒绝（`max_entry_bytes`） | ✅ |
| `hugerepeat/hugespan/emptyrowrepeat--errors.ods` | 拒绝 | 拒绝 | 拒绝，但**理由是 calamine 认不出格式**，不是预算命中 | ⚠️ 假阳性 |
| `deepnest--errors.ppt` | 拒绝 | 拒绝 | 拒绝（格式不支持） | ⚠️ 假阳性 |
| `corrupt-styles--skips.docx` | **恢复** | rc 0 | **rc 4 硬失败**（`Unexpected EOF`） | ❌ 违反计划 §6.4 规则 2 |
| `mismatched--recovers.docx` | **恢复** | rc 0 | **rc 4 硬失败**（`Expecting </w:p>`） | ❌ 同上 |
| `missing-styles--skips.docx` | 恢复 | rc 0 | rc 0 | ✅ |
| `unclosed--recovers.docx` | 恢复 | rc 0 | rc 0 | ✅ |
| `unbalanced--recovers.rtf` | 恢复 | rc 0 | rc 0 | ✅ |
| `encrypted--errors.odt` | 拒绝 | 拒绝 | 拒绝（`Encrypted`） | ✅ |
| `empty/truncated--errors.docx` | 拒绝 | 拒绝 | 拒绝 | ✅ |

净结果：**2 例该拒未拒（真实 DoS 面）、2 例该恢复却硬失败、2 例拒绝理由不正确**。

### 3.4 性能（native 领先，这是当前唯一确定优势）

小样本上两者都是亚毫秒级，CLI 墙钟被进程启动完全淹没（本机启动基线：uparser ≈ 63 ms，anydoc ≈ 66 ms），无区分度。因此构造大输入实测（各 3 次，取稳定值）：

| 输入 | anydoc 墙钟 | native 墙钟 | 扣除启动后（≈65 ms） | native 相对 |
|---|---:|---:|---|---:|
| `big.csv`（6.0 MB / 12 万行 / 4 列） | 536–564 ms | **430–438 ms** | 475 ms vs **373 ms** | **1.27× 更快** |
| `big.rtf`（3.9 MB / 6 万段落 + 行内样式） | 272–303 ms | **167–178 ms** | 221 ms vs **109 ms** | **2.03× 更快** |

两者对 `big.rtf` 的输出**字节数仅差 1**（2,868,893 vs 2,868,894），说明大体量纯文本路径上语义等价——native 是在同等输出下更快，不是靠少做事更快。

> 计时口径声明：Windows 逐文件 CLI 墙钟，扣除各自实测启动基线。这只支持"量级/倍数级"判断，不构成精确基准；正式基线需按 §5 的 N-12 建 in-process bench。DOCX/XLSX/EPUB 的大输入吞吐本次**未测**（本机无 `zip`/Python，无法合成大 OOXML 包），是已知缺口。

### 3.5 综合判定

| 维度 | 胜方 | 差距 |
|---|---|---|
| 现代格式成功率 | **anydoc** | 100% vs 85% |
| 格式广度 | **anydoc** | +DOC/PPT（7 样本 native 全败） |
| 行内样式保真 | **anydoc** | native 丢失字符样式 |
| 列表保真 | **anydoc** | native 丢 marker/层级/续编号 |
| EPUB 内容完整性 | **anydoc** | native 表格内容全丢 |
| RTF 结构保真 | **anydoc** | native 结构错乱 |
| 表格 span 保真 | **native** | canonical grid + HTML 降级 |
| 结构化 IR / document-json | **native** | anydoc 的 PDF 绕过 IR，且无 schema version |
| 异常输入拒绝 | **anydoc** | native 2 例漏拒 |
| 破损输入恢复 | **anydoc** | native 2 例误拒 |
| **吞吐性能** | **native** | **1.27×–2.03×** |
| 产品闭环（PDF/VLM/路由/cache） | **native** | anydoc 无此层 |

---

## 4. 根因归纳（只有 5 个，全部是"公共层没建"）

1. **没有 OPC 关系层** → altpath 全败 + PPTX 误检 + 格式判定不可靠。（计划 §6.1，未做）
2. **没有统一 XML 层** → 预算散落各前端、docx/pptx 漏检 depth/asset/text、破损 XML 无法容错续解析。（计划 §6.2/§6.4，未做）
3. **renderer 是骨架** → 样式标记失配、列表退化、转义过度/不足、标题重复。（计划 §10，未做）
4. **ODS 走了 calamine 而非 ODF walker** → 最小合法 ODS 失败，且 ODF 的 repeat/span 预算对 ODS 完全失效。（计划 §8.4，路由错）
5. **没有真实语料测试** → 上面 4 条全部逃过 21 个绿色测试；EPUB 表格内容全丢这种级别的缺陷也没被拦住。（计划 §12，未做）

---

## 5. 执行方案

### 5.1 排序原则

先修**根因公共层**再修格式细节——上面 13 个具体失败里有 9 个来自 4 个公共层缺失。若先逐个格式打补丁，会在 renderer/package 重构时全部返工（这正是执行计划 §18 已经警告过的）。

### 5.2 波次与任务

#### 波次 0 — 门禁（0.5 周）· 必须先做

| ID | 任务 | 验收 |
|---|---|---|
| N-0.1 | 修复 `epub.rs` 编译错误（已本地修复，待提交） | `cargo build --workspace --features native` 通过 |
| N-0.2 | 建 CI：`fmt` + `clippy -D warnings` + `build (default/native)` + `test` | 不可编译/不过 clippy 的提交无法合入 |
| N-0.3 | 引入固定语料回归脚本 `bench/document-corpus/run.sh`，输出 §3.1/§3.3 那两张表 | 每次 PR 自动产出成功率与 abuse/malformed 判定表 |
| N-0.4 | **语料授权决策**：直接复用 anydoc fixtures（MIT，需在 `ATTRIBUTION.md` 登记来源/commit/hash），或按规范自行生成等价样本 | 有明确 ADR；若自生成，须覆盖 altpath/strict/对抗 content-types/最小 ODS 等同类特征 |

**Gate G-0：** CI 绿；语料脚本可一键复现本文全部数字。

> N-0.4 需要你拍板：**复用 anydoc 的 fixtures（快、且是现成的对抗样本）**，还是**按 ECMA-376/ODF 规范自行生成（干净，但要多花约 1 周且容易漏掉对抗特征）**。执行计划 §12.2 写的是"不得依赖 anydoc/tests 作为运行期或测试依赖"——复用需要一次显式的 vendor+attribution 决策。

#### 波次 1 — OPC/Package 层重建（1.5 周）· 修 4/6 现代格式失败

| ID | 任务 |
|---|---|
| N-1.1 | `package.rs` 扩展为计划 §6.1 全量 API：`required_part`/`optional_part`/`xml_part`/`relationships_for`/`resolve_target`/`media_type_for`，part 读取缓存 + 重复引用只计费一次 |
| N-1.2 | 新增 `package/path.rs`：斜杠归一、dot segment、fragment、百分号解码后拒绝分隔符与 traversal |
| N-1.3 | `detect.rs` 改为**关系驱动**：根 `_rels/.rels` → `officeDocument` 关系 → 目标 part 的 Content-Type **精确匹配**（不再子串匹配、不再固定 wordprocessing 优先） |
| N-1.4 | DOCX/PPTX 主 part 与所有从属 part（styles/numbering/notes/slides/notesSlides）改为经 relationship 定位，惯例路径仅作 fallback 并发 warning |
| N-1.5 | 格式嗅探本身加预算（当前 `detect_zip_package` 无限额打开容器，且与 `Package::open` 重复解 zip 两次） |

**Gate G-1：** `handmade-altpath.docx`、`handmade-altpath.pptx`、`handmade-order.pptx`、`handmade-strict.pptx` 全部成功；PPTX/DOCX/XLSX 误判为 0；现代格式成功率 ≥ 95%（38/40）。

#### 波次 2 — XML 层与恢复政策（1.5 周）· 修全部健壮性缺陷

| ID | 任务 |
|---|---|
| N-2.1 | 新增 `package/xml.rs`：namespace 归一、编码声明、实体、**depth/node/text 预算集中强制**；所有前端改为经它读 XML，禁止直接 `quick_xml::Reader` |
| N-2.2 | 实现计划 §6.4 恢复政策：optional part（styles/numbering/notes/媒体）解析失败 → `ParseWarning` + 继续；正文 XML 局部失配 → 尽力续解析并 `TruncatedContent` warning；`ResourceLimit`/`Encrypted` 永远硬失败 |
| N-2.3 | 资产预算改为**累计**而非单文件；文本输出预算全前端接通 |
| N-2.4 | ZIP 预算从"声明大小"改为"实际读取累计 + 压缩比上限"（当前只信 header 声明值，可被伪造） |

**Gate G-2：** abuse 8/8 正确拒绝且**拒绝理由与命中的预算一致**；malformed 中 5 个 `--recovers` 全部 rc=0 且带 warning，5 个 `--errors` 全部 typed error；`deepxml--errors.docx`、`hugespan--errors.pptx` 不再被接受。

#### 波次 3 — Renderer 重写（1.5 周）· 修全部质量退化

| ID | 任务 |
|---|---|
| N-3.1 | 拆为 `render/markdown/{mod,escape,inline,table,anchors}.rs` |
| N-3.2 | 上下文感知转义（段落 / 行首 / link label / URL / table cell / backtick 分别处理），取消当前全局 `\ * _` 替换 |
| N-3.3 | 相邻 style run 合并 + **空白边缘外提**（修 `**\nbold**` 失配） |
| N-3.4 | 列表：嵌套缩进、marker 类型（decimal/alpha/roman）、`start`、跨段续编号 |
| N-3.5 | anchor slug 化 + 重复 id 去重；内部链接（含 EPUB 跨章 href）解析到统一 slug |
| N-3.6 | `unit.label` 不再无条件输出 `# `：仅当该 unit 首块不是同文标题时输出，且 Sheet/Slide 用可配置层级（修 EPUB 标题重复 / 标题过检） |
| N-3.7 | code fence 动态长度；`header_rows == 0` 的表不再把首行当表头 |

**Gate G-3：** `text.docx`/`text.odt`/`text.rtf` 三份输出**互相**归一化后相似度 ≥ 0.95（跨格式契约，无需外部 GT）；`book.epub` 输出对 `fixture-src/book.md` 的标题数、列表项数、表格单元格文本全部匹配；无 `**\n` 类失配标记。

#### 波次 4 — 格式前端补齐（2 周）

| ID | 任务 |
|---|---|
| N-4.1 | DOCX 字符样式：`rStyle` + 字符样式 basedOn 链（修行内样式全丢） |
| N-4.2 | DOCX 多级列表（当前扁平化 + warning）、numbering override、`lvlOverride`/`startOverride` |
| N-4.3 | **EPUB 表格单元格文本丢失**（本次最严重缺陷，优先级等同 P0） |
| N-4.4 | ODS 改由 `odf.rs` 的 table walker 处理（脱离 calamine），复用 repeat/covered/预算逻辑；calamine 仅保留 XLS/XLSX/XLSM/XLSB |
| N-4.5 | RTF：style table 驱动的标题/列表边界还原（修结构错乱） |
| N-4.6 | PPTX layout/master 继承、几何阅读顺序 fallback（计划 §8.3 未做部分） |

**Gate G-4：** 现代格式成功率 40/40；`ods` 3/3；跨格式契约（docx/odt/rtf 同源）相似度 ≥ 0.97。

#### 波次 5 — 产品接入闭环（1 周）

| ID | 任务 |
|---|---|
| N-5.1 | `ParseOptions` 贯通到 CLI/API/绑定：`--limits-*`、`--include-assets/notes/headers-footers`、`--fidelity semantic\|visual\|auto` |
| N-5.2 | 消除 `run_parse_native` 的**双重解析**（`--format document-json` 当前解析两遍） |
| N-5.3 | `compatibility_block` 重写：表格走 `html` 保留 rowspan/colspan、`reading_order` 按 unit 内线性顺序填充、List 保留 `list` category、去掉"每 block 重建 CanonicalDocument"的 O(n) 全量渲染 |
| N-5.4 | 结构化文档 asset 落盘（复用 `assets.rs`），Markdown 里 `![](images/<sha>.png)` 而非当前的 `asset-xxxx` 悬空 id |
| N-5.5 | CanonicalDocument 级 cache（key 含 parser family + engine schema version + options 稳定序列化） |
| N-5.6 | `Asset.bytes` 从 document-json 默认剔除（当前 `Option<Vec<u8>>` 会被序列化成 JSON 数字数组，体积膨胀数倍），改为默认只出 metadata + path，`--embed-assets` 显式开启 |
| N-5.7 | 删除 `ingest.rs` 的 `structured_bypass_xlsx/csv`（保留一个版本周期的 deprecated shim） |

**Gate G-5：** CLI 三种输出（markdown/json/document-json）均只解析一次；document-json 无内联字节数组；XLSX/CSV/DOCX/PPTX 的 assets 真实落盘并可在 Markdown 中打开。

#### 波次 6 — 测试与语料体系（1 周，可与 4/5 并行）

| ID | 任务 |
|---|---|
| N-6.1 | 每个格式前端建自己的单元测试（当前 5 个前端 0 测试） |
| N-6.2 | snapshot 测试（Markdown + document-json + warnings） |
| N-6.3 | malformed / abuse 语料纳入 CI，断言**具体的 typed error 与 warning code**，而非只断言"失败" |
| N-6.4 | mutation：每 fixture 25 轮确定性字节变异，允许 typed error，禁止 panic/hang/OOM |
| N-6.5 | `cargo-fuzz` target：detect / xml / rtf / csv / table builder |
| N-6.6 | 跨格式契约测试（同源 docx/odt/rtf/epub 输出等价） |

**Gate G-6：** 单测覆盖每个前端；mutation 与 fuzz smoke 进 CI；**新增一条硬规则：修任何缺陷必须先补一个会失败的语料级测试**（本次 EPUB 表格空单元格能逃过 21 个绿测，就是因为断言只到形状不到内容）。

#### 波次 7 — 性能基线守护（0.5 周，与波次 3 同步启动）

| ID | 任务 |
|---|---|
| N-7.1 | 建 in-process bench（criterion），区分 core parse / render / end-to-end，报 median + p95 + peak RSS；**不要再用 CLI 墙钟做基线** |
| N-7.2 | 合成大输入语料：CSV / RTF（已有）+ DOCX / PPTX / XLSX / EPUB（本次因缺 zip 工具未测，属已知缺口） |
| N-7.3 | 每波次结束跑一次，回归阈值 10% |

**Gate G-7：** 六种格式的大输入吞吐 native **全部 ≥ anydoc**；当前已达成 CSV 1.27×、RTF 2.03×，需在波次 1–4 增加公共层后**守住不低于 1.0×**。

#### 波次 8（可选，独立里程碑）— legacy DOC/PPT（8–12 周）

维持执行计划 §8.7 的判断：`legacy-office` experimental feature，不阻塞现代格式发布。若短期需要 DOC/PPT 覆盖，可先在 `auto` 路由里对这两种格式降级到现有 LibreOffice→VLM 路径，并明确告警。

### 5.3 时间与并行

| 波次 | 串行工时 | 可并行 |
|---|---:|---|
| 0 门禁 | 0.5 周 | — |
| 1 OPC/Package | 1.5 周 | — （其余全部依赖它） |
| 2 XML/恢复 | 1.5 周 | 与波次 3 可并行（不同文件） |
| 3 Renderer | 1.5 周 | 与波次 2 并行 |
| 4 格式前端 | 2 周 | 内部按格式可 3 人并行 |
| 5 产品接入 | 1 周 | 与波次 4 后半并行 |
| 6 测试体系 | 1 周 | 全程并行 |
| 7 性能 | 0.5 周 | 全程并行 |

- **单人串行：约 9.5 周**
- **三人并行：约 5–6 个日历周**（波次 1 必须先单人冻结公共层）
- 加 legacy DOC/PPT：再 +8–12 周

### 5.4 总验收（"native 全面超过 anydoc"的定义）

| 指标 | 当前 | 目标 |
|---|---:|---:|
| 现代格式成功率 | 34/40 (85%) | **40/40 (100%)** |
| abuse 正确拒绝（且理由正确） | 4/8 | **8/8** |
| malformed `--recovers` 恢复率 | 3/5 | **5/5** |
| 跨格式契约相似度（docx/odt/rtf 同源） | 未测（肉眼可见大幅发散） | **≥ 0.97** |
| `book.epub` 对 `book.md` 表格单元格文本 | **0%** | **100%** |
| 行内样式保真（bold/italic/strike） | 丢失 | 与 GT 一致 |
| 大输入吞吐 vs anydoc（6 种格式） | CSV 1.27× / RTF 2.03×，其余未测 | **全部 ≥ 1.0×，CSV/RTF 不低于当前** |
| 前端单测覆盖 | 5 个前端 0 测试 | 每前端有单测 + snapshot + mutation |
| 格式广度 | 缺 DOC/PPT | 波次 8 或显式降级路径 |

---

## 6. 风险

| 风险 | 影响 | 对策 |
|---|---|---|
| 波次 1 重构 Package 波及全部前端 | 大面积回归 | 先建 N-0.3 语料回归脚本，重构过程中每次提交对照成功率表 |
| Renderer 重写导致既有输出全变 | 下游/快照全红 | 波次 3 之前先补 snapshot（N-6.2），把"变了什么"变成可审阅的 diff 而非未知 |
| 复用 anydoc fixtures 的许可与"clean-room"冲突 | 法务/维护争议 | N-0.4 显式 ADR：vendor + attribution，或规范自生成 |
| 补齐质量后性能优势被吃掉 | 失去唯一优势 | 波次 7 与波次 3 同步启动，每波次卡 10% 回归阈值 |
| ODS 脱离 calamine 的工作量被低估 | 波次 4 延期 | ODS 表格走查与 ODT/ODP 共用 `odf.rs` walker，实际是复用而非新写；先做最小 ODS（`mimetype`+`content.xml`）验证 |
| legacy DOC/PPT 拖住发布 | 长期无可发布结果 | 保持独立里程碑，现代格式先发 |

---

## 附录 A：复现命令

```bash
# 构建
cd uparser && cargo build --release --features native -p uparser-core --bin uparser
cd opensource/anydoc && cargo build --release --example convert

# 全语料对拍（§3.1）
bash /tmp/cmp/run.sh          # 见本文档 §1 口径；建议固化到 bench/document-corpus/run.sh

# 单文件
uparser/target/release/uparser.exe parse <f> --protocol native --format markdown --no-assets
opensource/anydoc/target/release/examples/convert.exe <f> -o out.md

# 大输入性能（§3.4）
# big.csv: 12 万行 4 列；big.rtf: 6 万段落含 \b/\i
```

## 附录 B：本次实测原始结论清单

1. HEAD `dc99cc8` 编译失败（`epub.rs:31`，E0382）。
2. 修复后 `cargo test --release -p uparser-document-engine` 21/21 通过。
3. 47 个格式样本：anydoc 47/47 成功，native 34/47；现代格式 40 个中 native 34 个。
4. native 失败明细：doc×4、ppt×3（计划内）、ods×2、docx×1、pptx×3（**非计划内**）。
5. 3 个 PPTX 报 `missing required part: word/document.xml` = 被误判为 DOCX（对抗性 content-types）。
6. 2 个 ODS 报 `Cannot detect file format` = calamine 拒绝最小合法 ODS。
7. `text.docx`：native 丢失全部 bold/italic/strike；列表编号/层级/marker/续编号全部退化。
8. `text.rtf`：native 标题变列表项、style 标记跨行断裂、列表全部压平。
9. `book.epub`：native 表格 4 个单元格文本全空；章节标题重复输出；内部链接为死链。
10. `handmade-tables.docx`：native 的 HTML 表格比 anydoc 的 GFM 降维**保留了更多内容**（native 占优）。
11. abuse：`deepxml--errors.docx`、`hugespan--errors.pptx` 被 native 接受（rc 0），应拒绝。
12. malformed：`corrupt-styles--skips.docx`、`mismatched--recovers.docx` 被 native 硬失败，应恢复。
13. 性能：big.csv native 373 ms vs anydoc 475 ms（1.27×）；big.rtf native 109 ms vs anydoc 221 ms（2.03×），均已扣除进程启动。
