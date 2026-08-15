# uparser Native 多格式引擎实施状态

> 更新日期：2026-08-15
> 本文记录 `NATIVE_MULTIFORMAT_ENGINE_EXECUTION_PLAN.md` 的首个可发布里程碑。

## 1. 已交付架构

新增独立 workspace crate：

    uparser/crates/uparser-document-engine

该 crate 不依赖 anydoc，也未复制或 vendor anydoc 源码。它采用相同类型的分层思想：

    内容检测
      -> 受限容器读取
      -> 格式前端
      -> CanonicalDocument
      -> Markdown / document JSON / ParseResult 兼容映射

现有 `uparser-native-engine` 继续只负责 PDF。`uparser-core` 的 native adapter 根据内容格式选择 PDF 引擎或结构化文档引擎。

## 2. 当前支持矩阵

| 格式 | native 状态 | 当前语义覆盖 |
|---|---|---|
| PDF | 已支持，保持原路径 | 文本层、页面、坐标、表格及原生 Markdown |
| CSV/TSV | 已支持 | BOM/UTF-8/UTF-16、分隔符探测、引号、类型和表头推断 |
| XLS/XLSX/XLSM/XLSB | 已支持 | 多 sheet、单元格类型、公式、XLS/XLSX 合并单元格 |
| ODS | 已支持 | 多 sheet、单元格类型和公式；合并单元格仍有限 |
| DOCX | 已支持基础源语义 | 标题、段落、换行、列表、表格 |
| PPTX | 已支持基础源语义 | slide 顺序、标题、段落、项目符号、speaker notes |
| DOC/PPT | 仅检测，未解析 | 后续 legacy OLE 波次 |
| ODT | 已支持 | metadata、标题、段落、编号/项目列表、链接、脚注/尾注、图片、repeat/span 表格 |
| ODP | 已支持基础源语义 | 每个 draw:page 一个 slide，标题、段落、列表、表格、图片 |
| EPUB | 已支持基础源语义 | OPF metadata/manifest/spine、XHTML 章节、样式、链接、列表、图片、引用、代码块、表格 |
| RTF | 已支持 | ANSI/Shift-JIS/Cyrillic/Unicode、样式、列表、表格、field、note、bookmark、pict |
| PNG/JPEG/扫描件 | 不属于源语义解析 | 继续由显式 VLM/OCR 协议处理 |

“已支持”表示 native 可以无 LibreOffice、无 PDFium、无模型服务完成文本解析，不表示像素级复刻 Office 排版。

## 3. 关键行为变化

1. `native` 从 PDF 专用协议扩展为本地确定性解析协议。
2. `auto` 对 CSV、TSV、Excel、ODS、DOCX、PPTX、ODT、ODP、EPUB、RTF 优先选择 native 源语义解析。
3. 显式指定 VLM/OCR 时，不再由 XLSX/CSV 的旧 structured bypass 抢占协议。
4. 未启用 `native` feature 的构建暂时保留旧 structured bypass，避免默认构建行为突然失效。
5. 结构化结果协议名包含前端，例如 `native:csv`、`native:excel`、`native:docx`。
6. `routed_by` 现在能正确区分显式 native 与 auto 路由。

## 4. CanonicalDocument 合同

统一 IR 已包含：

- Flow、Page、Slide、Sheet、Chapter 逻辑单元；
- Heading、Paragraph、List、Table、BlockQuote、CodeBlock、Figure；
- 行内样式、链接、锚点、note 引用、公式；
- 单元格类型、公式、rowspan、colspan 和 covered slot；
- footnote、endnote、comment、speaker note；
- asset 元数据、内容哈希和可选 bytes；
- typed terminal error 与 recoverable warning。

Markdown renderer 对普通表格输出 GFM，对带合并单元格的表格输出 HTML table，避免 span 信息丢失。

## 5. 安全边界

当前已实施以下预算：

- 输入总大小；
- ZIP entry 数量；
- 单 entry 解压大小；
- ZIP 声明的总解压大小；
- XML event 数量；
- 表格展开单元格数量；
- 解码文本大小。

Office 前端只读取白名单 XML part，不执行宏、脚本、OLE 对象或外部关系。ZIP 工作簿在交给 calamine 前先经过统一包预算预检。

## 6. 当前不足

DOCX 已处理 paragraph style 的 basedOn/outline 继承、numbering.xml 的 marker/start、run 样式、内部锚点与 hyperlink relationships、图片、脚注/尾注及横纵合并单元格。多级列表目前仍扁平化并产生 warning；批注、修订、header/footer、复杂 field 和完整 style cascade 尚未实现。

PPTX 已按 presentation relationships 确定 slide 顺序，并处理 graphicFrame 表格、合并单元格、图片 asset 和 relationship notes。layout/master 继承、chart、SmartArt、几何阅读顺序、theme 字体及复杂 placeholder 过滤仍未实现。

Excel 不计算公式，只保留公式和缓存值；XLSB/ODS 的合并单元格覆盖不如 XLS/XLSX 完整。

对外 `ParseResult` 仍是兼容层，无法表达 CanonicalDocument 的全部嵌套语义。CLI 已增加 `--format document-json`，Rust API 已增加 `parse_canonical_document`，Node 已增加 `parseDocument`，Python 已增加 `parse_document`。

资源预算目前基于 ZIP 声明大小，后续还应增加实际读取累计值和压缩比限制，并加入恶意样本 corpus。

## 7. 验证结果

- `uparser-document-engine`：13 个单元/集成测试通过。
- `uparser-core --features native`：317 个库测试、29 个 CLI 测试、2 个合同测试通过；跳过 2 个在 Windows 上硬编码 Unix `sleep` 的既有测试。
- 默认核心构建：305/307 个库测试通过，29 个 CLI 和 2 个合同测试通过；相同 2 个平台测试失败。
- Clippy：`uparser-document-engine`、`uparser-core --features native` 和 `uparser-napi` 均以 `-D warnings` 通过。
- CLI 实跑：显式 native Markdown、auto JSON、document-json 均成功解析 CSV；document-json 输出 `schema_version: uparser.document.v1`。
- Node 绑定编译通过；Python 绑定代码已接入，但当前机器没有 Python 3，PyO3 构建脚本无法完成环境验证。

## 8. 下一波次

1. 完成 DOCX 多级列表、comments、headers/footers、fields 和更完整的 style cascade。
2. 完成 PPTX layout/master、charts、几何阅读顺序和 theme 继承。
3. 增强 ODP master/layout、EPUB nav/NCX/footnote/MathML/CSS 语义。
4. 增加 assets manifest 与结构化文档 asset 落盘。
5. 建立多格式 golden corpus、损坏包、加密包和资源攻击测试。
6. 最后评估 legacy DOC/PPT 的纯 Rust 实现成本与兼容范围。
