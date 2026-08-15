# uparser Native 多格式文本解析引擎技术架构与执行方案

> 方案日期：2026-08-15  
> 目标：参考 anydoc 的分层思想，但不依赖 anydoc crate、不 vendor anydoc 源码，使 uparser native 支持 Office、OpenDocument、RTF、EPUB、CSV 和 PDF 的本地结构化文本解析。  
> 原则：纯 Rust 优先、源结构优先、单一语义 IR、统一 Markdown、无外部服务、可按页/区域回退 VLM。

## 1. 执行摘要

本方案建议新增独立 workspace crate：

    uparser/crates/uparser-document-engine

它与现有 uparser-native-engine 分工：

- uparser-native-engine：继续负责 PDF 文本层、坐标、版面、表格和 PDF Markdown。
- uparser-document-engine：负责 DOCX/DOCM、PPTX/PPTM/PPSX、XLS/XLSX/XLSM/XLSB、ODT/ODS/ODP、RTF、EPUB、CSV/TSV，以及后续 DOC/PPT。
- uparser-core：负责格式检测、协议选择、缓存、错误契约、资产落盘、VLM 回退和对外 API。

不应继续把每种格式直接拼成现有 Page/Block。现有 Block 强制带 Geometry，缺少嵌套块、列表、note、anchor、cell span 和 embedded asset，是面向页面/VLM 的 IR，不足以承载 Office 源结构。

正确路线是：

    bytes
      -> 内容级格式检测
      -> 安全容器层
      -> 格式前端
      -> CanonicalDocument（唯一富语义 IR）
         -> 统一 GFM renderer
         -> uparser ParseResult 兼容映射
         -> document-json / assets

native 对外语义扩展为“本地确定性解析协议”，而不再等同于 PDF 协议：

- PDF：uparser-native-engine。
- 结构化文档：uparser-document-engine。
- 图片或扫描件：native 返回 needs_ocr；auto 路由 VLM。
- 显式 VLM：保留现有 normalize/rasterize 路径，用于视觉保真而不是源结构解析。

### 推荐支持顺序

| 波次 | 格式 | 原因 |
|---|---|---|
| Wave 1 | CSV/TSV、XLS/XLSX/XLSM/XLSB | 已有 calamine/csv 基础，可先建立 IR 和 renderer |
| Wave 2 | DOCX/DOCM | 业务价值最高，可验证 package、style、list、table、note |
| Wave 3 | PPTX/PPTM/PPSX/PPSM | 复用 OOXML package、relationships、DrawingML |
| Wave 4 | ODT/ODS/ODP、EPUB | 复用 ZIP/XML/HTML 和 canonical table/list |
| Wave 5 | RTF | 独立 lexer/state machine，复杂但无容器依赖 |
| Wave 6 | DOC/PPT | OLE/FIB/record 规格复杂，最后实施 |

一名高级 Rust 工程师完成 Wave 1-5、测试和产品接入，合理预估为 18-24 工程周；加入 legacy DOC/PPT 需要再增加 8-12 工程周。三人并行团队可在 12-16 个日历周交付 Wave 1-5 的稳定版本，但前提是公共层先冻结。

## 2. 当前实现审计

### 2.1 已有能力

当前 uparser 已具备：

- DocumentFormat：PDF、DOCX、PPTX、XLSX、CSV、PNG、JPEG。
- XLSX structured bypass：calamine 读取 worksheet，输出一个 HTML table Block。
- CSV structured bypass：csv crate 读取 rows，输出一个 HTML table Block。
- DOCX/PPTX：依赖 LibreOffice 转 PDF，再光栅化，交给 VLM。
- PDF native：纯 Rust pdf-inspector lineage。
- 多个 VLM/OCR adapter、Profiler/Router、cache、assets、JSON/Markdown renderer。

### 2.2 当前问题

1. **structured_bypass 是控制流特例。** 它在协议选择之前执行，所以即便用户显式指定 VLM，XLSX/CSV 仍直接旁路；协议语义不清晰。
2. **XLSX/CSV 输出过度降维。** sheet 名、header、merge、cell 类型、公式、logical unit 信息都丢失，只剩 HTML 字符串。
3. **DOCX/PPTX 依赖外部 LibreOffice。** 增加部署、启动、超时和子进程治理成本；转换到 PDF 后还丢失源列表、note、link、asset 等确定结构。
4. **现有 Page/Block 不适合作为 Office parser 内部 IR。** Geometry 是必填，嵌套结构和关系信息不足。
5. **当前 renderer 太简单。** 只对 title/list 做单层标记，无法统一渲染嵌套列表、footnote、anchor、rowspan/colspan、inline style。
6. **格式检测过度依赖 file-format 的扩展结果。** 无法完整区分 OLE 内的 DOC/PPT/XLS、ZIP 内的 OOXML/ODF/EPUB 变体和宏格式。
7. **错误类型只偏向 rasterize/conversion。** 缺少 encrypted、malformed part、resource limit、unsupported feature、partial recovery 等本地 parser 契约。

结论：不能在 ingest.rs 中继续增加 parse_docx、parse_rtf 等函数。必须先建立独立引擎、富语义 IR 和公共 package 层。

## 3. 目标与非目标

### 3.1 必须达成

1. native 无需 LibreOffice、ImageMagick、模型服务即可解析支持的结构化格式。
2. 每种格式只负责 bytes -> CanonicalDocument，不直接拼 Markdown。
3. Markdown、document-json、现有 ParseResult 都从同一 CanonicalDocument 派生。
4. 内容检测优先于扩展名，错扩展名仍能解析；CSV/TSV 等无签名格式例外。
5. 不可信 ZIP/XML/OLE/RTF 输入有固定资源预算，ResourceLimit 不得被容错吞掉。
6. malformed optional part 可跳过并产生 warning；无法形成有意义内容时返回 typed error。
7. embedded asset 以 bytes/media type/来源 part 保留，并复用 uparser assets 落盘能力。
8. auto 模式优先源结构解析；无法解析或需要视觉语义时可显式回退 VLM。
9. default build 不引入 PDFium、LibreOffice 或系统原生库。
10. 核心 API 能被 NAPI/Python 使用，且长解析释放 GIL/不阻塞 Node event loop。

### 3.2 暂不承诺

- 不重建 Word/PPT 的像素级页面布局。
- 不执行宏、JavaScript 或 embedded executable。
- 不实现 Excel 公式计算引擎。
- 不保留完整 tracked changes、comment workflow、动画和 PowerPoint transition。
- 不对扫描图片做 OCR；由现有 VLM/OCR 协议承担。
- 首版不承诺 DOC/PPT 100% 规格覆盖。

## 4. 目标组件架构

### 4.1 Workspace 结构

    uparser/
      crates/
        uparser-core/
          src/
            ingest/
              mod.rs
              detect.rs
              dispatch.rs
              fallback.rs
            adapters/
              native.rs
              native_document.rs
            render/
              legacy.rs
        uparser-native-engine/       # PDF
        uparser-document-engine/     # 新增
          Cargo.toml
          src/
            lib.rs
            error.rs
            options.rs
            model/
            package/
            formats/
              csv.rs
              sheet.rs
              docx/
              pptx/
              odf/
              epub/
              rtf/
              doc/
              ppt/
            shared/
            render/
              markdown/
          tests/
          fuzz/

### 4.2 依赖边界

uparser-document-engine 可以依赖通用底层 crate，但不得依赖 anydoc：

| 能力 | 推荐依赖 | 用途 |
|---|---|---|
| ZIP/deflate | zip | OOXML、ODF、EPUB |
| XML pull parser | quick-xml | namespace-aware XML |
| OLE compound file | cfb | DOC/PPT/XLS 格式检测与 legacy parser |
| Spreadsheet | calamine | XLS/XLSX/XLSM/XLSB 值和 merge |
| CSV | csv、encoding_rs | delimiter/encoding/quoted cell |
| 编码 | encoding_rs | RTF、binary Office、CSV |
| 日志 | log/tracing facade | recover/skip 事件 |
| 错误 | thiserror | 稳定 typed errors |
| 哈希 | sha2 | source/asset identity |

版本应在 workspace.dependencies 统一，避免 core 与 document engine 各编译一份 calamine/csv。

### 4.3 Clean-room 原则

“参考 anydoc 架构”只使用公开架构思想：

- frontend -> shared model -> renderer；
- package/relationships/path/limits 公共层；
- canonical table grid；
- typed errors 和统一恢复政策。

具体实现基于 ECMA-376、ISO/IEC 29500、ODF、RTF、MS-DOC/MS-PPT 等规范和底层 crate API重新编写。不得直接复制 anydoc 源文件。若未来决定复用 MIT 代码，必须转为明确 vendor/attribution 决策，不能混在本方案中。

## 5. CanonicalDocument 设计

### 5.1 为什么不能直接扩展现有 Block

现有 Block 被所有 VLM adapter 大量构造，新增必填字段会造成广泛迁移；它还把坐标和内容耦合。Office 内容没有稳定 page/bbox，强行填零坐标会污染语义。

因此采用双层 IR：

- CanonicalDocument：document engine 内唯一信息完整 IR。
- ParseResult：现有兼容 IR，由 lowering 生成，不作为格式 parser 的工作模型。

### 5.2 建议类型

    CanonicalDocument
      metadata
      units: Vec<DocumentUnit>
      notes: Vec<Note>
      assets: Vec<Asset>
      warnings: Vec<ParseWarning>

    DocumentUnit
      kind: Flow | Page | Slide | Sheet | Chapter
      index
      label
      blocks: Vec<DocBlock>

    DocBlock
      Heading { level, anchor, inlines }
      Paragraph { inlines }
      List { marker, start, items }
      Table { grid, header_rows, kind }
      BlockQuote { blocks }
      CodeBlock { language, text }
      Figure { asset, alt, caption }
      Rule

    Inline
      Text { text, style }
      Link { target, content }
      Image { asset/source, alt }
      Anchor
      NoteRef
      LineBreak
      Formula { source, display }

    Style
      bold, italic, strike, code, underline, superscript, subscript

DocumentUnit 解决非 PDF 的逻辑分页问题：

- DOCX/RTF/ODT：默认一个 Flow unit；可选按显式 page break 分段，但不伪造物理页码。
- PPT/PPTX/ODP：一张 slide 一个 Slide unit。
- XLS/ODS/CSV：一个 worksheet 一个 Sheet unit。
- EPUB：一个 spine item 一个 Chapter unit。
- PDF：继续是 Page unit，但第一阶段不强制迁移 PDF engine 到 CanonicalDocument。

### 5.3 Canonical table invariant

Table 必须是显式二维网格：

- Origin cell 保存 blocks、row_span、col_span。
- Covered cell 回指 origin。
- 一个逻辑位置只出现一次。
- 所有 span 在展开前计入 resource budget。
- 重叠 span 按格式恢复政策钳制或 warning，不允许产生不一致网格。
- header_rows 与 TableKind（Data/Layout）独立保存。

### 5.4 资产与链接

Asset：

- id：文档级稳定索引。
- media_type。
- original_part/name。
- sha256。
- bytes。

LinkTarget：

- External URL。
- Relative source link。
- Internal anchor。

同一 part 被引用多次必须去重，不能重复解压和重复计入 asset budget。

### 5.5 对现有 ParseResult 的 lowering

新增 document_bridge.rs：

- 一个 DocumentUnit 映射为一个现有 Page。
- Page.page_num 使用 unit.index。
- Page.width_px/height_px 为 0，但 capability_notes 明确 logical unit 不是物理页。
- block.geom 使用 Rect([0,0,0,0])，bbox_px=None。
- Heading -> category=title、MergeHint::TitleLevel。
- Paragraph -> text。
- List -> 递归渲染为 block text 或新增 html；不得仅丢成一个 list 标记。
- Table -> HTML，保留 rowspan/colspan。
- Figure -> asset bytes/path。
- source=StructuredNative。
- reading_order 按 unit 内 block 线性顺序填充。

这只是兼容层。新增 CLI 输出 document-json，直接序列化 CanonicalDocument，提供无损结构 API。后续 IR v2 再考虑合并两套类型。

## 6. 公共容器与安全层

### 6.1 Package API

Package 必须封装所有 ZIP 访问，格式 parser 不直接操作 ZipArchive：

- open(bytes, limits)
- required_part(path)
- optional_part(path)
- xml_part(path)
- relationships_for(part)
- resolve_target(base, target)
- media_type_for(part)

读取过的 part 缓存为共享 bytes；重复引用只计费一次。

### 6.2 ResourceLimits

建议默认值由配置结构承载，而不是散落常量：

| 限额 | 初始建议 |
|---|---:|
| 输入文件 | 256 MiB |
| 单解压 part | 128 MiB |
| 总解压 | 512 MiB |
| archive entries | 100,000 |
| XML 深度 | 256 |
| XML nodes | 2,000,000 |
| 表格/repeat 展开位置 | 4,000,000 |
| retained assets | 128 MiB |
| 文本输出 | 64 MiB |
| OLE record depth | 64 |
| OLE/RTF records/tokens | 16,000,000 |

服务层还应有 wall-clock timeout 和 cancellation token。ResourceLimit 是 fatal error，不能被 optional part 的容错逻辑吞掉。

### 6.3 路径安全

- 统一斜杠和根路径。
- 正确解析 dot segment 和 fragment。
- 拒绝百分号解码后产生路径分隔符。
- 拒绝编码后的 dot traversal。
- external relationship 不得尝试网络读取。
- 宏和 OLE executable 只作为不可执行 asset metadata，默认不保留高风险 payload，或由 options 显式允许。

### 6.4 Error 与恢复政策

DocumentError：

- UnsupportedFormat。
- Malformed { part, detail }。
- Encrypted。
- ResourceLimit { limit, detail }。
- MissingPart { part }。
- Io。

ParseWarning：

- OptionalPartSkipped。
- BrokenRelationship。
- UnsupportedFeature。
- StyleCycle。
- TruncatedContent。
- InvalidSpanClamped。
- AssetDropped。

恢复规则：

1. 主内容 part 缺失且无正文：error。
2. styles/numbering/notes/image 等 optional part 损坏：warning + 继续。
3. ResourceLimit/Encrypted：永远 error。
4. 至少产生有意义 block 才返回成功。
5. warning 要进入 CanonicalDocument，lowering 后写入 ParseResult.warnings。

## 7. 格式检测设计

不得只依赖扩展名或 file-format 的单一 extension。detect.rs 按以下顺序：

1. 固定签名：PDF、RTF、OLE。
2. OLE stream：
   - WordDocument -> DOC。
   - PowerPoint Document -> PPT。
   - Workbook/Book -> XLS。
3. ZIP：
   - ODF/EPUB mimetype。
   - OPC Content Types。
   - package root relationships。
   - fallback 到关键 part/root namespace。
4. 文本：
   - 显式 format option。
   - 文件名 hint：csv/tsv。
   - 可选 delimiter sniff，不对任意文本自动宣称 CSV。

DocumentFormat 扩展为：

    Pdf, Doc, Docx, Ppt, Pptx, Excel,
    Odt, Ods, Odp, Rtf, Epub, Csv,
    Png, Jpeg, Unknown

宏容器归并 parser family，但 metadata 保留原扩展/variant。

检测必须先做便宜的 envelope 检查，再受限打开容器，避免 format sniff 本身成为资源攻击面。

## 8. 各格式实施方案

### 8.1 Wave 1：CSV/TSV 与 Excel

#### CSV/TSV

改造当前旁路：

- BOM 检测：UTF-8/UTF-16LE/UTF-16BE。
- delimiter sniff：逗号、tab、分号、竖线；只看有限前缀。
- quoted multiline 由 csv crate 处理。
- 输出 canonical Table，不再直接拼 HTML。
- 第一行 header 推断必须可配置；默认保守。

#### Excel

使用 calamine 的 auto workbook reader支持 XLS/XLSX/XLSM/XLSB：

- 每个 worksheet -> Sheet unit。
- sheet 名进入 label。
- cell 保存可见值和 value kind。
- merge region -> canonical span。
- 空白 leading/trailing rows/columns 有预算地裁剪。
- 日期、时间、duration、浮点按稳定规则格式化。
- 公式首版只输出 cached/display value；可选 metadata 保存公式文本，绝不执行公式。

**Gate W1**

- XLS/XLSX/XLSM/XLSB/CSV/TSV 全部走 native，不走 structured_bypass。
- Markdown 与 document-json 均来自同一 IR。
- merge、引号、UTF-16、多 sheet snapshot 通过。
- 现有 XLSX/CSV CLI 行为保持兼容，protocol 改为 native:excel/native:csv。

### 8.2 Wave 2：DOCX/DOCM

模块：

- opc.rs：main part、typed relationships、strict/transitional namespace。
- styles.rs：default/basedOn/direct properties。
- numbering.rs：abstractNum、num override、level、counter。
- content.rs：paragraph、run、hyperlink、bookmark、field、drawing、table。
- notes.rs：footnote/endnote。
- assets.rs：DrawingML/VML/OLE object。

实现顺序：

1. paragraph/run/heading。
2. style cascade。
3. list/numbering。
4. table/merge/header。
5. links/bookmarks。
6. notes。
7. images/objects。
8. AlternateContent 和 strict namespace。

不将页眉/页脚默认混入正文；可通过 options.include_headers_footers 开启。tracked changes 默认接受最终可见内容，并 warning 说明未保留 revision history。

**Gate W2**

- 标题、嵌套编号、合并表、超链接、footnote、image fixture 全通过。
- DOCX 不需要 LibreOffice。
- 对 100+ 真实 DOCX 成功率至少 98%，panic 为 0。
- 与 LibreOffice rendered truth 做抽样人工对照，与 anydoc/pandoc 做非依赖 benchmark 对照。

### 8.3 Wave 3：PPTX/PPTM/PPSX/PPSM

复用 OOXML package、relationships、DrawingML 和 table：

- presentation relationship 决定 slide 顺序。
- slide -> layout -> master 做 placeholder/style cascade。
- title placeholder -> Heading。
- body text/list 按 shape/source order。
- notes slide 作为 slide 尾部 notes 或独立 metadata。
- graphicFrame table -> canonical Table。
- image -> Figure/Asset。

自由 shape 的阅读顺序首版采用：

1. semantic placeholder 顺序优先。
2. 无 placeholder 时按 top-to-bottom、left-to-right。
3. 保留 shape index/provenance，后续可用几何 reading-order。

**Gate W3**

- 多 master、strict OOXML、notes、table、image、错序 rel fixture。
- 每张 slide 一个 logical unit。
- 不依赖 LibreOffice。
- 人工抽查 title/body/notes 顺序，失败可定位到 shape id。

### 8.4 Wave 4A：ODT/ODS/ODP

公共模块：

- manifest/encryption。
- named/automatic/default style。
- ODF text/list/frame。
- ODF table/repeat/covered cell。

注意 repeat count 和 span 必须先计预算，再展开。ODS 与 Excel 使用同一 canonical Table。

**Gate W4A**

- ODT heading/list/table/image/note。
- ODS gap/repeat/merge/date/duration。
- ODP title/body/table/notes。
- encrypted 输入返回 Encrypted。
- huge repeat/span 在固定时间内 ResourceLimit。

### 8.5 Wave 4B：EPUB

- container.xml 找 rootfile。
- OPF manifest/spine 决定 chapter 顺序。
- XHTML parser 转 DocBlock。
- 相对链接和 fragment 按 chapter scope。
- CSS 只实现文本语义必要子集，不做 layout engine。
- fixed-layout EPUB 产生 capability warning。

HTML walker应作为 shared/html 单独实现，供未来 HTML 输入复用。

**Gate W4B**

- EPUB2/3、namespace、spine、CSS link、internal anchor、image。
- 不执行 script、不发网络请求。
- chapter 顺序和内部链接稳定。

### 8.6 Wave 5：RTF

RTF 不能用字符串替换实现，必须：

- 零拷贝 lexer：GroupStart/End、ControlWord、ControlSymbol、Text、Binary。
- 栈式 parser state。
- codepage/font table/style/list table 预解析。
- Unicode fallback skip。
- field/note/bookmark capture frame。
- picture hex/binary collector。
- nested table state。
- suppressed destination allow/deny list。

每个 token、group depth、binary length 都计资源预算。

**Gate W5**

- ANSI/Shift-JIS/Cyrillic/Unicode。
- list、table、field、footnote、bookmark、pict。
- unbalanced group 可恢复，深度/超大 binary 受限。
- mutation corpus 不 panic、不 hang。

### 8.7 Wave 6：DOC/PPT

这是最高风险波次，不应阻塞现代格式发布。

DOC：

- OLE/FIB。
- CLX piece table。
- compressed ANSI/UTF-16 text。
- CHPX/PAPX/STSH/SPRMs。
- PlfLst/PlfLfo。
- PLC notes/bookmarks。
- OfficeArt pictures。

PPT：

- Current User。
- persist directory。
- record tree。
- master/slide/notes。
- style text。
- OfficeArt。

应先发布 experimental feature：

    legacy-office = ["doc", "ppt"]

**Gate W6**

- malformed/truncated fixture typed error或明确 recover。
- 多编码、多 master、notes、list/table/image。
- 真实样本成功率达标后再进入默认 native。

## 9. Native 协议与路由语义

### 9.1 取消 structured_bypass

structured_bypass_xlsx/csv 迁入 document engine，ingest 控制流改为：

    detect
      -> choose execution path
         -> native structured parser
         -> native PDF parser
         -> visual normalize/rasterize
         -> image/VLM

格式检测与协议选择分开，不允许格式检测函数偷偷决定最终 parser。

### 9.2 显式协议

| 用户选择 | 结构化格式行为 |
|---|---|
| --protocol native | 只走本地源结构 parser；不自动启动 LibreOffice/VLM |
| --protocol auto | 优先 native；unsupported/needs_visual 时按 policy 升级 VLM |
| --protocol mineru-vlm 等 | 明确走 normalize -> rasterize -> 指定视觉协议 |

这样用户可在“源语义解析”和“视觉保真解析”之间明确选择。

### 9.3 Auto policy

- CSV/Excel/EPUB/RTF：native 优先。
- DOCX/ODT：native 成功且无 fatal warning即返回；若用户要求 layout fidelity，再走视觉协议。
- PPTX/ODP：默认 native；复杂自由布局或大量图表可标记 visual_recommended。
- PDF：沿用 PDF classification，文本型 native，扫描/乱码 VLM。
- image：VLM/OCR。

新增 ParseOptions：

- fidelity = semantic | visual | auto。
- fallback = none | on_unsupported | on_low_quality。
- include_assets。
- include_notes。
- include_headers_footers。
- limits。

### 9.4 Cache

cache key 必须加入：

- parser family。
- uparser-document-engine schema/version。
- ParseOptions 的稳定序列化。
- output contract version。
- source hash。

Markdown 和 document-json 可以共享 CanonicalDocument cache，避免重复解析。

## 10. Renderer 方案

新增 document engine 统一 GFM renderer，不复用当前简化 Page/Block renderer作为语义 renderer：

- context-aware Markdown escaping。
- 相邻 style run 合并。
- heading level clamp 1-6。
- nested list、start number、task item。
- internal anchor/relative/external link。
- footnote/endnote 编号。
- code fence 动态长度。
- canonical table -> GFM；复杂 span 可选择 HTML table。
- figure asset link和 alt/caption。
- logical unit 可选分隔标记。

表格渲染策略：

- 无 span、简单 cell -> GFM pipe table。
- 有 rowspan/colspan 或嵌套 block -> HTML table。
- JSON 始终保留 canonical grid，不受 Markdown 降维影响。

禁止各格式 parser 自行 escape 或拼 Markdown。

## 11. API、CLI 与兼容性

### 11.1 Rust API

uparser-document-engine：

- detect_format(bytes, hint)。
- parse_document(bytes, format, options) -> CanonicalDocument。
- to_markdown(document, render_options)。
- parse_to_markdown(bytes, format, options) 作为 convenience API。

uparser-core：

- parse() 继续返回 ParseResult，保持现有调用方。
- 新增 parse_document() 返回 versioned document DTO。

### 11.2 CLI

建议：

    uparser parse file.docx --protocol native --format markdown
    uparser parse file.xlsx --protocol native --format json
    uparser parse file.epub --protocol native --format document-json
    uparser inspect file.docx

inspect 输出：

- detected format/variant。
- parser family/version。
- units、blocks、tables、notes、assets 数。
- warnings。
- 是否建议 visual fallback。

### 11.3 JSON versioning

document-json 顶层必须有：

    schema_version: "uparser.document.v1"

所有后续新增字段应 serde default；删除/改义必须升级 schema major。现有 ParseResult JSON 不立即破坏。

### 11.4 NAPI/Python

首版可以只经 uparser-core 暴露，不为 document engine 单独发包：

- Node 异步任务进入 worker。
- Python 解析阶段释放 GIL。
- assets bytes 可选择不跨语言复制，先只返回 path/metadata。

## 12. 测试战略

### 12.1 测试金字塔

1. 单元测试：path、XML namespace、style cascade、numbering、grid、escaping。
2. 生成 fixture：每个格式针对单一特性构造最小文件。
3. snapshot：Markdown、document-json、warning/error。
4. malformed corpus：missing part、bad rel、unclosed XML/RTF、truncated OLE。
5. abuse corpus：zip bomb、deep XML、huge repeat/span、image bomb、deep record。
6. mutation：每个 fixture 做确定性字节变异，允许 typed error，不允许 panic/hang/OOM。
7. fuzz：detect、XML、RTF、DOC/PPT、CSV、table builder分别 target。
8. cross-format contract：等价内容在 DOCX/ODT/RTF/EPUB 输出等价 IR/Markdown。
9. end-to-end CLI/API/NAPI/Python。

### 12.2 Fixture 所有权

不得依赖 opensource/anydoc/tests 作为运行期或测试依赖。uparser 自己维护：

    uparser/crates/uparser-document-engine/tests/fixtures

fixture 可用脚本从公开规范最小结构生成；若引入第三方样本，登记来源、许可证和 hash。

### 12.3 Gate

每个波次合并要求：

- cargo fmt。
- clippy -D warnings。
- default/native/all relevant features build。
- 单测和 snapshot 通过。
- fuzz smoke。
- 资源滥用测试。
- 真实 corpus 成功率和性能不退化。

## 13. Benchmark 方案

### 13.1 两套 benchmark 分离

OpenDataLoader Bench 只用于 PDF native 回归，不能衡量 DOCX/PPTX/RTF。

新增 uparser-document-bench：

- 按格式分桶，不以某种格式样本数量支配总分。
- ground truth 包含可确定结构：plain text、heading、list、table grid、link、notes、assets。
- 每格式至少 30-50 个可再分发样本，另有内部真实 corpus。
- 对不可再分发 corpus 只发布聚合，不作为唯一证据。

### 13.2 指标

| 维度 | 指标 |
|---|---|
| 文本完整度 | normalized edit similarity / trigram containment |
| 标题 | precision、recall、level accuracy |
| 列表 | item F1、nesting accuracy、marker/start accuracy |
| 表格 | cell text、grid shape、rowspan/colspan、TEDS |
| 链接 | target/text precision、recall |
| notes | reference/body matching |
| assets | count、media type、hash retention |
| 稳定性 | success/recover/error/panic/OOM |
| 性能 | core parse、render、end-to-end，median/p95、peak RSS |

Overall 应先按格式、再按能力宏平均，避免 DOCX 数量多掩盖 PPT/RTF 退化。

### 13.3 对照

- 当前 uparser LibreOffice/VLM 路径。
- pandoc/LibreOffice。
- anydoc 仅作为外部竞争对照，可安装独立二进制运行，但不能成为构建或测试依赖。
- markitdown/docling 等按支持格式比较。

### 13.4 性能门槛

初始建议：

- CSV/Excel median 小于 20 ms（小文档）。
- DOCX/PPTX/ODF median 小于 50 ms。
- RTF median 小于 30 ms。
- EPUB median 小于 50 ms。
- 单文档峰值内存不超过输入解压后预算的可解释范围。
- 相同源 bytes 重复解析命中 CanonicalDocument cache 后小于 5 ms。

性能报告必须区分 core parse 与 CLI process startup。

## 14. 分阶段任务与交付门禁

### P0：架构冻结（1-2 周）

- 建新 crate、feature、error/options。
- CanonicalDocument v1。
- Package/limits/path/XML 骨架。
- document Markdown renderer 骨架。
- lowering 到 ParseResult。
- ADR：协议语义、logical unit、JSON schema、clean-room。

**G0：** 用内存构造 CanonicalDocument，Markdown/document-json/ParseResult 三种输出契约通过；不修改 VLM adapter 行为。

### P1：CSV/Excel 重构（1-2 周）

- 迁移 calamine/csv 到新 crate。
- Excel variants、encoding、delimiter、merge、sheet label。
- 删除 structured bypass 的格式实现，只保留过渡 shim。

**G1：** Wave 1 Gate 全过，现有 CLI 测试兼容。

### P2：DOCX（3-4 周）

- OPC/XML/relationships。
- style/list/content/table/note/assets。
- 真实语料、fuzz、resource abuse。

**G2：** DOCX 不装 LibreOffice也可 native 解析；成功率/结构指标达标。

### P3：PPTX（2-3 周）

- 复用 OOXML 公共层。
- master/layout/slide/notes/table/image/order。

**G3：** PPTX 全本地，slide logical unit 稳定。

### P4：ODF + EPUB（3-4 周）

- ODF styles/text/table/repeat。
- EPUB OPF/spine/XHTML。

**G4：** ODT/ODS/ODP/EPUB Gate 全过。

### P5：RTF（3-4 周）

- lexer/state machine/tables/fields/notes/pictures。
- mutation/fuzz/abuse。

**G5：** RTF 多编码和 malformed 稳定。

### P6：产品接入与发布（2-3 周，可与 P4/P5 交叠）

- native/auto/explicit VLM 语义。
- cache、CLI、API、NAPI/Python。
- document-json/inspect。
- benchmark、文档、迁移告警。

**G6：** 现代格式正式发布，无外部工具依赖；PDF benchmark 不退化。

### P7：Legacy DOC/PPT（8-12 周，独立里程碑）

- experimental feature。
- 规格子集和真实 corpus。
- 稳定后再默认启用。

## 15. 代码迁移清单

### uparser-core

1. ingest.rs 拆为 detect/dispatch/fallback。
2. DocumentFormat 扩展并迁移 content-based detection。
3. 删除 structured_bypass_xlsx/csv；保留一个版本周期 deprecated shim。
4. NativeAdapter 拆分 PDF 与 structured document dispatch。
5. API/CLI 的 native 特判统一到一个 service 层，消除重复逻辑。
6. render/mod.rs 保留 VLM/legacy ParseResult renderer；结构文档走 document engine renderer。
7. cache 支持 CanonicalDocument。
8. ParseResult warnings/capability notes 接入 document warnings。

### Cargo features

建议：

    native-pdf = ["dep:uparser-native-engine"]
    native-documents = ["dep:uparser-document-engine"]
    native = ["native-pdf", "native-documents"]
    legacy-office = ["uparser-document-engine/legacy-office"]

现代结构格式默认随 native；legacy DOC/PPT 初期不默认。

## 16. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 新 IR 与现有 Page/Block 冲突 | API 破坏或持续降维 | 双层 IR + versioned document-json + 兼容 lowering |
| 范围过大 | 长期无可发布结果 | 按 Wave 发布，legacy 独立 |
| OOXML/ODF producer 方言 | 兼容性波动 | relationship/path fallback、warning、真实 corpus |
| ZIP/XML/OLE 攻击面 | OOM/CPU DoS | 统一 Package + ResourceLimits + cancellation |
| renderer 复写复杂 | Markdown 回归 | 单一 renderer、context escaping、snapshot |
| anydoc 架构过度模仿 | 法务/维护争议 | clean-room ADR，不复制源码 |
| calamine 能力边界 | 公式/格式/merge 差异 | 明确 cached value 契约，缺失结构自行补 part parser |
| PPTX 阅读顺序不唯一 | 内容乱序 | placeholder 优先、geometry fallback、visual_recommended |
| DOC/PPT 规格复杂 | 延期/低成功率 | experimental feature，最后实施 |
| native 行为改变 | 现有用户惊讶 | 发布说明、protocol 字段版本化、可显式 fidelity |

## 17. 完成定义

现代多格式 native v1 完成必须同时满足：

1. PDF、DOCX/DOCM、PPTX/PPTM/PPSX/PPSM、XLS/XLSX/XLSM/XLSB、ODT/ODS/ODP、RTF、EPUB、CSV/TSV 均有本地 parser。
2. 不依赖 anydoc、LibreOffice、ImageMagick、外部服务或 PDFium。
3. 每种格式先进入 CanonicalDocument，再输出 Markdown/JSON。
4. 所有格式有内容检测、typed errors、resource limits、malformed/abuse tests。
5. 现有 ParseResult/Markdown 调用方保持兼容。
6. document-json 是无损结构主契约，并有 schema version。
7. auto 能在 structured native、PDF native 和 VLM 之间做可解释路由。
8. 可再分发 benchmark 按格式发布质量、成功率、延迟和内存。
9. PDF OpenDataLoader Bench 不因本项目退化。
10. Node/Python/CLI 端到端通过，解析不会阻塞宿主线程。

Legacy v2 完成再增加 DOC/PPT 的稳定支持，不应把它作为现代格式 v1 的发布阻塞项。

## 18. 推荐的第一批实际提交

为控制评审面，第一阶段不要直接写 DOCX parser，按以下 6 个 PR 推进：

1. **ADR + crate skeleton**：CanonicalDocument、DocumentUnit、DocBlock、errors/options，无格式实现。
2. **Renderer + bridge**：手工 IR 到 Markdown/document-json/ParseResult，锁定契约。
3. **Package security**：limits、path、ZIP、XML、relationships，加 abuse tests。
4. **Format detection**：完整 DocumentFormat、OLE/ZIP/content detection。
5. **CSV/Excel frontend**：迁移现有能力并增强 merge/sheet/encoding。
6. **Core integration**：native/auto/explicit VLM dispatch、cache、CLI。

完成这 6 个 PR 后再启动 DOCX。否则 DOCX 开发过程中会反复改变 IR、错误策略和 renderer，造成大量返工。

## 19. 最终建议

本项目不应以“给 ingest.rs 多加几个解析函数”为目标，而应建立一个可独立测试和演进的结构文档引擎。anydoc 最值得借鉴的不是具体代码，而是三个架构决定：

1. 格式前端只负责恢复源语义。
2. 所有格式收敛到一份有不变量的文档模型。
3. Markdown 和安全策略集中实现一次。

uparser 还需要在此基础上多做一层：把语义文档与已有页面/VLM 体系并存，通过 logical unit、兼容 lowering 和可解释 fallback 接入统一产品。按本方案实施后，native 才会从“PDF 文本层协议”升级为真正的“本地多格式文档解析协议”，同时不引入 anydoc 运行时或源码依赖。
