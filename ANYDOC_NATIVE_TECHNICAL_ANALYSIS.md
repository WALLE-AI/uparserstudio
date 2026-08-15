# anydoc 源码技术解读、uparser native 对比与 OpenDataLoader Benchmark 评估

> 分析日期：2026-08-15  
> 源码基线：仓库提交 97c8c96ed8cebfbe2ffc5227fc8865277bdf294b；anydoc 0.1.9；uparser-native-engine 0.1.7  
> 范围：opensource/anydoc、uparser native engine/adapter、benchmark/opendataloader-bench  
> 证据分级：源码静态分析、仓库历史实测、本次复跑观察和工程推断分别标注。

## 1. 结论摘要

1. **anydoc 是多办公格式的确定性结构转换器，不是 OCR 或视觉版面模型。** DOC/DOCX、PPT/PPTX、Excel/ODS、ODF、RTF、EPUB、CSV 分别解析后进入共享 Document IR，再由一个 GFM renderer 输出 Markdown。PDF 是唯一例外：直接委托 pdf-inspector 输出 Markdown，不进入共享 IR。
2. **anydoc 的优势是格式广度、输出一致、毫秒级延迟和低部署成本。** 核心约 66 个 Rust 文件、约 1.6 万行，没有模型服务、GPU 或系统 Office 依赖。它适合混合办公文档进入 RAG/LLM，不适合扫描件、视觉版面重建和坐标级文档理解。
3. **uparser native 与 anydoc 的 PDF 能力同源。** uparser 将 pdf-inspector commit 3fb5452 内部化为约 7.1 万行的 uparser-native-engine；anydoc 的 PDF 分支依赖 pdf-inspector 1.14.2。因此二者在 PDF 算法层面不是两条独立路线，差异主要在版本、封装、产品边界和 IR。
4. **uparser native 当前并未在 Markdown 质量上超过 pdf-inspector。** native Markdown 直接返回 vendored engine 的结果。历史评测二者 Overall 都是 0.8754，输出逐字节一致；native 的价值是去掉旧 liteparse/PDFium 依赖并接入 uparser CLI、Profiler/Router 和 JSON IR。
5. **OpenDataLoader Bench 证明 native 是很强的无模型电子 PDF 基线，但不能证明通用解析能力。** 该集只有 200 个单页 PDF；NID 覆盖 200 篇、TEDS 42 篇、MHS 107 篇。历史 native 为 NID 0.9150、TEDS 0.8141、MHS 0.7875、Overall 0.8754。
6. **本次当前二进制复跑 200 篇耗时 7.646 秒，即 0.0382 s/doc，零进程失败，但有 1 篇空输出。** 当前输出有 280 个标题，GT 只有 193 个，说明标题过检仍很明显。
7. **native 应优先修复表格、标题过检、词内空格、扫描回退和双轨 IR。** Benchmark 的 MHS 特意忽略 H1-H6 层级，因此调整井号深度几乎无收益。

## 2. 产品定位与总体架构

### 2.1 anydoc 数据流

    输入 bytes/path
        |
        +-- 内容检测：PDF / RTF / OLE / ZIP OPC / ODF / EPUB
        |
        +-- 格式前端
        |     DOC/DOCX/RTF/ODF/PPT/PPTX/Excel/EPUB/CSV
        |                    |
        |                    v
        |              共享 Document IR
        |                    |
        |                    v
        |              统一 GFM renderer
        |
        +-- PDF -> pdf-inspector -> Markdown（绕过 Document IR）

anydoc 优先读取源格式已经编码的结构，例如 OOXML 样式、列表定义、合并单元格和关系引用，而不是从页面像素重新推断结构。它与 Docling、MinerU 或 VLM 的目标不同。

### 2.2 uparser native 数据流

uparser native 是 PDF 专用零模型协议，有两个出口：

- Markdown：NativeAdapter.native_markdown 调 vendored engine 的完整 Markdown pipeline。
- JSON：parse_document 获取 TextItem，adapter 自己按 Y 容差聚行，再映射为 uparser Block。

因此 native 不是“一份 IR、多种 renderer”。Markdown 和 JSON 在结构能力、排序逻辑和输出语义上分叉，形成一致性风险。

## 3. anydoc 源码模块深度分析

### 3.1 API 与静态分发

src/lib.rs 的公开面很小：Format 检测、to_document、to_markdown。路径 API 优先按内容识别，扩展名仅用于 CSV 等无签名格式回退。src/formats/mod.rs 用 match 静态分发 12 个 parser family。

优点是低复杂度、低运行期开销、易审计；代价是新格式必须同步修改核心枚举、Node/Python/WASM 的格式映射和类型定义。PDF 在 to_markdown_bytes 中特判，to_document 明确不支持 PDF，这是最重要的架构断点。

### 3.2 格式检测：src/formats/detect.rs

- PDF：文件头签名。
- RTF：RTF open group。
- OLE2：compound file 流名区分 Word、PowerPoint、Excel。
- ZIP：先看 ODF/EPUB mimetype，再读 OPC Content Types；必要时按根元素或关键 part 路径推断。
- CSV：没有可靠魔数，必须显式命名。

这是面向真实脏数据的合理实现，能处理错扩展名和 OOXML strict/transitional 变体。ZIP 探测仍会打开不可信容器，所以真正安全边界在 package 限额，而非魔数。

### 3.3 容器与安全层：src/package

| 模块 | 职责 | 关键设计 |
|---|---|---|
| archive.rs | ZIP/OLE part 读取 | 解压缓存、重复引用去重、总量计费、OLE 探测 |
| xml.rs | XML 树 | namespace 归一、编码声明、实体、节点/深度限制 |
| relationships.rs | OPC 关系 | 内外链区分、按 part 解析 rels、资源降级 |
| path.rs | 包内路径 | 相对路径、fragment、百分号解码、拒绝编码分隔符与 traversal |
| limits.rs | 固定预算 | 单 entry 128 MiB、总解压 512 MiB、10 万 entry、XML 深度 256、200 万节点、展开 400 万位置、asset 128 MiB |

错误策略是“可恢复 part 问题记录日志并跳过，ResourceLimit 永远硬失败”。ConvertError 提供 Unsupported、Malformed、Encrypted、ResourceLimit、MissingPart、Io 和稳定机器码，比静默返回空 Markdown 更适合生产治理。

这些是单文档预算，不是服务级预算。多并发接近上限的文档仍需要宿主限制并发、请求体和超时。

### 3.4 统一文档模型：src/model

Document 只含 blocks、notes、assets。Block 有 Heading、Paragraph、List、Table、BlockQuote、CodeBlock、Rule；Inline 有 Text、Link、Image、Anchor、NoteRef、LineBreak。Style 已解析为 bold/italic/strike/code 布尔值。

表格是 IR 最强的部分。Table.grid 中每个位置要么是 Origin cell，要么是 Covered 并回指 origin。所有前端通过 GridBuilder 建表；重叠 span 被钳制，巨大 span 在展开前计费。这样 renderer 不需要重复处理每种格式的异常合并单元格。

IR 的明确边界也很重要：没有页面、bbox、字体名、置信度、公式 AST、批注、修订历史、图表语义和无障碍结构树。它是内容语义 IR，不是版面 IR。

### 3.5 DOCX：src/formats/docx

解析链是 package relationships -> main part -> styles/numbering -> body -> footnotes/endnotes：

- 从 package relationship 找主文档，不死绑 word/document.xml，但保留惯例路径回退。
- 样式按默认值、basedOn 链、段落/字符 direct properties 的规范顺序解析。
- 编号先解析 abstractNum、num 和 style 绑定，再用文档级 counter 生成源编号；列表不是靠文本正则猜测。
- content.rs 按源顺序交错处理 inline 与 text box/chart 等块附件。
- 支持字段、bookmark、内外链、图片、VML/DrawingML、表格、footnote/endnote。
- Markup Compatibility 按支持 namespace 选择 Choice/Fallback。
- OLE object 作为 asset 保留，不把 preview image 当对象本体。

不足是只保留接受后的内容，不保留 tracked changes、comment thread、section/page layout 和完整页眉页脚编辑语义。

### 3.6 PPTX：src/formats/pptx

它按 presentation relationship 顺序加载 slide，再解析 layout 和 master，做 placeholder 与文本样式 cascade。标题 placeholder 转 Heading，正文保留段落/列表，notes slide 和 graphicFrame table 进入统一模型。

这比简单抽 XML 文本可靠，但它不是视觉画布重建器：不表达绝对坐标、shape z-order、动画、主题色或完整 chart data。自由布局必然被线性化。

### 3.7 二进制 DOC/PPT

DOC 前端直接实现 OLE2、FIB、CLX piece table、压缩/UTF-16 piece、CHPX/PAPX FKP、STSH、SPRMs、PlfLst/PlfLfo、PLC notes 和 OfficeArt 图片。字符位置按 UTF-16 code unit 计数，与 CP-indexed PLC 对齐，这对多字节文本很关键。

PPT 解析 Current User、PowerPoint Document persist 目录和 record tree，加载 master text style、slide text、speaker notes 和 OfficeArt 图片。

优点是无 LibreOffice 依赖、启动快；风险是 MS-DOC/PPT 规格面巨大，当前是面向内容恢复的子集。少见 field/object/section、嵌套表格和私有 producer 记录可能降级或跳过，真实语料和 fuzz 尤其重要。

### 3.8 RTF：src/formats/rtf

RTF 使用 lexer 加栈式状态机。它先扫描 ANSI codepage，再解析 font/style/list table；正文维护 group state、Unicode fallback、field/note/bookmark frame、picture collector、嵌套表格和列表 run。

关键难点是 group unwind 时正确关闭捕获 frame，而不仅是 token 化。实现已覆盖 field result、footnote/endnote、listtext、pict 和 nested table properties。RTF 方言众多，对 Cocoa/Word 私有 destination 仍只能容错优先。

### 3.9 ODF：src/formats/odf

ODT/ODS/ODP 共用 package、style 和 text/table walker：

- 读取 content.xml、styles.xml、manifest，并检测加密。
- 合并 automatic、named 和 default styles。
- ODT 解析 heading、paragraph、list、link、bookmark、frame/image、table、notes。
- ODS 处理 repeated row/cell、covered cell、空白 gap、合并区域、日期/时长文本。
- ODP 按 presentation page 遍历 shape，标题转 Heading。

ODF repeat/span 是压缩炸弹高风险点，因此复用统一 expansion budget；测试中也有 huge repeat/span abuse fixture。

### 3.10 Excel 与 CSV

Excel 前端用 calamine 读取 xls/xlsx/xlsm/xlsb，逐 sheet 生成 Table；多 sheet 插入 sheet heading，补充 merged regions，并推断 header row。数值按电子表格有效精度格式化。

它优先输出可见值，不是工作簿计算引擎：不执行公式，不保留公式表达式、条件格式、隐藏状态、图表和 pivot 语义。用于 RAG 很合适，用于审计或工作簿重建不够。

CSV 处理 BOM、UTF-16/UTF-8 容错解码，按多行列数一致性嗅探 delimiter，再由 csv crate 处理 quoting。整个文件映射为一张表。

### 3.11 EPUB 与 HTML

EPUB 读取 container.xml -> OPF rootfile -> manifest/spine，按 spine 顺序解析 XHTML。内部 fragment 做 chapter scope，图片进入 assets。shared/html.rs 提供 HTML 到 model 的 walker。

它不是浏览器：不执行 JavaScript，不做完整 CSS layout。适合 reflowable EPUB，对 fixed-layout EPUB 和绝对定位内容会损失视觉顺序。

### 3.12 Markdown renderer

| 文件 | 作用 |
|---|---|
| anchors.rs | 解析被引用 anchor、GFM slug、重复 id 去重 |
| escape.rs | 段落、行首、link label、URL、table cell、backtick 的上下文转义 |
| inline.rs | 合并相邻 style run、处理边缘空白、避免标记错配 |
| table.rs | 裁尾空行列、压平 cell blocks、保留 covered slot、解包简单 layout table |
| mod.rs | notes 编号、block/list/blockquote/code 渲染和空块过滤 |

统一 renderer 是 anydoc 最大架构收益：转义或表格修复一次即可覆盖所有 Office 前端。但 GFM 本身无法忠实表达 rowspan/colspan，covered 位置只能输出为空，复杂/嵌套 cell 也必然降维。

### 3.13 shared 工具层

- assets：part 去重和 asset 总预算。
- blockstyle/delta/chain：样式增量和继承链。
- fields：Word/RTF field instruction。
- grid/header：边界表格和 header 推断。
- list/numbering：嵌套列表、罗马/字母/复合编号。
- drawingml/officeart：XML 与二进制 Office 图片公共逻辑。
- text/uri：空白清洗与链接分类。

这层使各格式前端共享行为，而不是复制近似实现。

### 3.14 Node、Python、WASM

- Node/N-API 将转换放入 libuv worker，避免阻塞 event loop。
- Python/PyO3 用 py.detach 释放 GIL；ConvertError 映射为细分异常，Io 保持 OSError。
- WASM 只有 bytes API；Document 经 serde 返回，错误对象附 code。

三套绑定重复维护 Format/model 映射，新 variant 有同步成本。WASM 还受浏览器内存约束，核心 512 MiB 解压上限对浏览器并不一定合理。

### 3.15 测试与 anydoc 自有 benchmark

测试包括 fixture snapshot、malformed outcome、abuse 限额、每 fixture 25 轮确定性字节变异、每格式 cargo-fuzz target，以及三种绑定测试。优点是同时覆盖输出回归和不 panic；缺口是约 100 个真实 samples 被 gitignore，CI 无法复现完整质量基线。

anydoc 自有 benchmark 不能和 OpenDataLoader Bench 混用。前者是 100 个、14 格式办公文档，以 LibreOffice 前 6 页截图为 ground truth，由 Claude Sonnet 5 双向盲评。README 报 anydoc 81/100、median 4.4 ms，但 corpus 不可再分发；bench/README.md 还明确 PDF 不在该 harness 范围。

## 4. uparser native 源码解读

### 4.1 引擎模块

uparser-native-engine 从 pdf-inspector 3fb5452 vendor，保留 MIT attribution，删除上游 CLI/Python binding。

| 模块 | 职责 | 技术要点 |
|---|---|---|
| detector.rs | PDF 分类 | TextBased/Scanned/ImageBased/Mixed、页级 OCR reason、confidence |
| extractor/content_stream.rs | 内容流 | 解释文本、矩阵、路径、图像 operator，生成 TextItem/line/rect |
| extractor/fonts.rs、tounicode.rs | 字体 | simple/Type0/CID widths、Encoding、ToUnicode、TrueType cmap、外部 CMaps |
| extractor/layout.rs | 版面 | span 合并、letter spacing、自适应组行、列检测、页码过滤 |
| extractor/reading_order.rs | 顺序 | column band、region graph、图像锚定、RTL |
| structure_tree.rs | Tagged PDF | StructTree/MCID、table row/cell、异常 name 修复 |
| tables | 表格 | structure、rectangle、vector line、text heuristic、financial split、grid |
| markdown | 结构渲染 | 字体标题、列表/code/caption、跨页表、重复页眉页脚、清理 |
| text_quality.rs | 质量门 | mojibake、CID garbage、低可用文本和 OCR 路由信号 |

表格是多信号融合：优先 tagged structure，其次矩形/线段网格，最后用文本对齐和财务 token 猜无边框表。0.814 TEDS 已明显强于纯文本基线，但误检/漏检仍是相对强模型的主要差距。

标题基于正文基准字号、font tier、粗体、稀有度、TOC/caption/fragment 过滤。没有 semantic tag 时只能启发式判断，280 vs 193 的过检正是边界。

### 4.2 NativeAdapter 的结构损失

JSON 路径没有复用 engine 的 TextLine、Table 或 Heading：

1. 获取平铺 TextItem。
2. 丢 Image 和空文本。
3. 按页，以 Y 降序、X 升序排序。
4. item 中心 Y 差不超过 max height 的 0.6 倍时合并成行。
5. 水平 gap 大于 font size 的 0.15 倍时插空格。
6. 用文本最大右/上边界估算页面宽高并翻转 Y。
7. 每行输出 category=text 的 Block。

结果缺少 heading/list/table/category、真实 MediaBox/CropBox、图片、链接类型、表单类型、confidence 和 OCR reason。provides_reading_order 返回 true，但 adapter 实际是全页 Y/X 排序，对两栏页面并不严格正确；reading_order 字段本身仍为 None。

### 4.3 部署价值

native feature 仅依赖 vendored engine，不再连带 PDFium；PDFium 只属于 VLM 光栅化 feature。纯 Rust、无服务和约 0.04 秒/文档的延迟，使 native 适合 auto router 首层：电子文本 PDF 直接处理，扫描/乱码/复杂布局升级 OCR/VLM。

但 CLI native 分支绕过通用 scheduler、cache、stream 和 postprocess，统一平台能力还没有完全闭环。

## 5. anydoc 与 uparser native 对比

| 维度 | anydoc | uparser native | 判断 |
|---|---|---|---|
| 定位 | 多 Office/ebook 格式到语义 Markdown | 电子文本 PDF 零模型协议 | 互补 |
| PDF 引擎 | pdf-inspector 1.14.2 | vendored pdf-inspector lineage | 算法同源，版本可能漂移 |
| 输入 | 12 parser family、20+ 扩展 | PDF | anydoc 广度高 |
| OCR | 无 | 无，但 uparser router 可升级 VLM | uparser 产品闭环更强 |
| IR | 语义 blocks/notes/assets，无坐标 | pages/blocks/spans/bbox，但 native 映射仅文本行 | 关注点不同 |
| PDF 到 IR | 不支持 | 支持简化行 IR | native 胜，但损失结构 |
| Markdown | Office 统一 renderer；PDF 例外 | engine 自有 renderer | PDF 本质同源 |
| 表格 | 源格式显式结构到 canonical grid | 几何/标签/启发式重建 | Office 更确定，PDF 必须推断 |
| 资产 | embedded bytes 与 media type | adapter IR 丢图片 | anydoc API 更完整 |
| 坐标 | 无 | TextItem/bbox | native 适合版面处理 |
| 安全 | 明确 archive/XML/expansion 限额 | 有 PDF 校验，但缺统一公开资源预算 | anydoc 边界更清楚 |
| 延迟 | README 4.4 ms/Office doc | 本次 38.2 ms/单页 PDF，含逐文件 CLI | 语料/口径不同，不可排名 |

合理整合不是让 native 重写 anydoc，而是 uparser 用 anydoc 处理 Office/EPUB/CSV，PDF 保持 native/VLM 分流；上层建立同时容纳 semantic block 和 page geometry 的 IR。

## 6. OpenDataLoader Bench 评估

### 6.1 数据和实现口径

本地有 200 个 PDF、200 个 GT Markdown。reference.json 虽含 element 坐标，主 evaluator 实际比较 Markdown：

- NID：代码实际用 RapidFuzz fuzz.ratio 比较 whitespace collapse 后文本；NID-S 先去 table。
- TEDS：pipe table 先转 HTML，对 DOM 做 APTED；比较 tag、rowspan、colspan 和 cell text，TEDS-S 忽略文本。
- MHS：识别 1 到 6 级标题，但把所有级别转成相同 heading tag，根节点下平铺 heading/content。因此不测标题级别，也不测真正嵌套层级。
- Overall：先对每篇可用的 NID/TEDS/MHS 求均值，再对 200 篇 overall 求均值，不是三个全局 mean 的简单平均。

覆盖量为 NID 200、TEDS 42、MHS 107。无表格/标题时返回 null，退出对应 aggregate。

### 6.2 优点

- 所有引擎统一成 Markdown，贴近 RAG 输出。
- NID-S/TEDS-S 部分解耦文本和结构。
- 保存 per-document 明细和 prediction，便于失败分桶。
- 固定 200 篇，适合作回归门禁。

### 6.3 主要局限

1. 语料是单页样本，无法评跨页段落/表格、重复页眉、流式和长文档内存。
2. MHS 名称误导：实现不看标题级别或真正 hierarchy。
3. 表格只有 42 篇，在全局 Overall 中实际权重远低于三分之一。
4. NID 同时受漏字、乱码、连字符、页眉和 serialization 影响，不是纯阅读顺序。
5. 速度来自不同硬件和包装口径：M4 in-process、Windows 逐文件 CLI、预热五轮不能精确横比。
6. 当前 checkout 未注册 uparser native，prediction 也没有其 evaluation.json；根报告的 0.8754 原始审计链不完整。
7. 当前 LiteParse 1.2.1 prediction 是纯文本，所以 TEDS/MHS 为 0。上游 markdown 口径 LiteParse 2.10.1 报 0.873；拿 0.576 作为结构基线会夸大 native 收益。
8. 没有 bootstrap CI、paired significance 或版面类型置信区间；0.875 和 0.882 不能解释为稳定排名。

### 6.4 历史结果的正确读法

| 引擎 | Overall | NID | TEDS | MHS | 来源 |
|---|---:|---:|---:|---:|---|
| uparser mineru-vlm | 0.9284 | 0.9470 | 0.9439 | 0.8777 | 根报告历史 GPU 实测 |
| OpenDataLoader hybrid | 0.9066 | 0.9337 | 0.9276 | 0.8208 | 当前 evaluation.json |
| Docling | 0.882 | 0.898 | 0.887 | 0.824 | bench README |
| uparser native | 0.8754 | 0.9150 | 0.8141 | 0.7875 | 根报告历史；engine 直通 |
| pdf-inspector | 0.8754 | 0.9150 | 0.8141 | 0.7875 | 同次历史；byte-identical |
| LiteParse 1.2.1 | 0.5756 | 0.8660 | 0 | 0 | 当前纯文本 prediction，不公平 |

可靠结论：native 在无模型 PDF 中阅读顺序和表格恢复很强，明显好于纯文本 adapter；它距离 hybrid/VLM 的表格与标题仍有差距。不能称为超过自身底座 pdf-inspector，也不能用该 PDF 集评价 anydoc Office 前端。

### 6.5 本次当前二进制复跑

命令口径：200 个 PDF 分别启动 uparser.exe parse --protocol native --format markdown --no-assets，产物在 output/opendataloader-native-20260815/markdown。

| 观察项 | native | GT |
|---|---:|---:|
| 文件数 | 200 | 200 |
| 进程失败 | 0 | - |
| 空 Markdown | 1 | 0 |
| 总耗时 | 7.646 s | - |
| 平均耗时 | 0.0382 s/doc | - |
| 含标题文档 | 131 | 107 |
| 标题数 | 280 | 193 |
| pipe-table 行 | 466 | 0 |
| HTML table 数 | 0 | 55 |
| 总字符数 | 385,202 | 442,941 |

pipe table 和 HTML table 计数不能直接比较，evaluator 会统一转换；但 280/193 直接支持标题过检。抽样 01030000000001 还出现 ar e、mod- el、inter- nal 等词内/换行断词，以及页眉被识别成标题。

本机没有 benchmark 所需 Python 3.13 和依赖环境，因此本次未重新算 NID/TEDS/MHS；质量分沿用历史实测并明确标注。

## 7. native 当前不足、根因和建议

### P0：扫描件/坏字体没有完整回退契约

引擎能给 PdfType、page OCR reasons 和 text quality，但 native_markdown 只取 markdown，扫描页可静默变空。本次已有 1 篇空输出。

建议返回 markdown、classification、page reasons、quality；auto 按页分流，显式 native 至少发 typed warning/error。验收：空输出为 0，不可解析必须明确失败；另测扫描/乱码路由 recall。

### P0：Markdown 与 JSON IR 双轨

Markdown 有标题、列表、表格、列顺序、页眉过滤；JSON 只有简单 Y/X 文本行。换 output format 会改变结构和顺序。

建议 engine 暴露稳定 NativeDocument，包括 page box、lines、headings、tables、images、links、reasons；一次解析后渲染 Markdown/JSON。验收：JSON flatten 与 Markdown 非表格文本 NID-S 大于 0.99，category 不再全是 text。

### P0：表格是最大质量差距

TEDS 0.8141 低于 hybrid 0.9276 和 mineru-vlm 0.9439。弱点主要是无边框、复杂 merge、跨栏和图表文字。

建议按 StructTree/rect/line/heuristic 保存 provenance 与 confidence，先分析 42 篇低分尾部；低置信表区域局部走 TSR/VLM。验收：TEDS 至少 0.86，TEDS-S 不下降，同时报告 table detection precision/recall。

### P1：标题过检，不是标题层级问题

当前 280 vs GT 193，常见来源是页眉、短粗体句、图表标签和大字号页码。MHS 忽略层级，所以压平 H1/H2 无效。

建议加入 repeated header/footer、边距位置、句末标点、跨页重复、正文密度和 StructTree role；输出 confidence。验收：标题数误差由 +45% 降到正负 10%，MHS 大于 0.82，并在独立文集审计 precision。

### P1：词内空格、连字符和字形解码

抽样有 ar e、mod- el。可能来自 font width、TJ adjustment、line merge 和 hyphen 规则。CJK/CID 依赖 external/bcmaps，分发遗漏会严重退化。

建议记录 join decision feature，建立 Canva、Type0/CID、ligature、hyphen、CJK/RTL regression；bcmaps 做启动自检。验收：NID 大于 0.92，并按语言/字体分桶。

### P1：page geometry 是估算，单位标注不准确

adapter 用文本最大 x/y 当 page width/height，丢 MediaBox/CropBox 空白；CoordinateSystem 标为 PixelAbs，实际接近 PDF point。旋转页、负坐标和 crop box 会错。

建议 engine 返回 MediaBox/CropBox/Rotate/UserUnit，IR 明确 PdfPoint 或按指定 DPI 转换。验收：页面尺寸误差小于 1 point/pixel，并覆盖四种旋转。

### P1：JSON reading order 声明与实现不一致

provides_reading_order 为 true，但全页 Y/X 排序会交错多栏，且 reading_order 字段为 None。

建议直接消费 engine region graph并填写 order；JSON 路径单独跑 NID，不能用 Markdown 分数代替。

### P1：安全与资源治理不足

anydoc 有明确 archive/XML/expansion 上限；native 虽有 PDF validation 和错误类型，但没有同等级公开的 object、stream、XObject depth、operator、page、output、CPU budget。

建议引入 ResourceLimits 和 cancellation，增加恶意 PDF mutation/fuzz corpus。

### P2：vendor 同步与重复依赖

uparser 固定在 2026-08-04 commit，anydoc 使用 pdf-inspector 1.14.2。同一产品同时引入两者可能编译两份 PDF core、输出不一致、安全修复漂移。

建议建立可执行 upstream sync/diff CI；考虑统一 workspace PDF crate。每次同步跑 byte diff、benchmark 和 security corpus。

### P2：benchmark 不可完整复现

当前缺 uparser adapter、evaluation.json、binary SHA、per-doc latency 和统一计时规则。

建议正式注册 engine，保存 git/binary SHA、CPU/OS、warmup/rounds 和 evaluation；至少五轮交替顺序，报告 median/p95；质量做 paired bootstrap CI。

## 8. 推荐路线与验收

### 阶段 A：契约和复现

1. 接入当前 bench registry 并提交 native artifacts。
2. 速度分为 in-process core time 和 end-to-end CLI time。
3. 空输出改 typed result，router 消费页级 OCR reason。
4. 明确 JSON 坐标 frame 和 page box。

### 阶段 B：统一结构 IR

1. engine 暴露 NativeDocument。
2. Markdown 和 JSON 从同一对象渲染。
3. category、reading_order、confidence、provenance 进入 Block。
4. Office 输入评估接入 anydoc Document adapter。

### 阶段 C：质量

1. 先修 heading precision 与 spacing/hyphen。
2. 表格按 detector provenance 做消融。
3. 低置信 table/scan/garbled 页面做局部 VLM 回退。
4. 增加 multi-page、CJK/RTL、rotate/crop、tagged/untagged、恶意 PDF held-out。

| 目标 | 当前 | 下一阶段门槛 |
|---|---:|---:|
| NID | 0.9150（历史） | 至少 0.920，JSON 路径另测 |
| TEDS | 0.8141（历史） | 至少 0.860 |
| MHS | 0.7875（历史） | 至少 0.820 |
| 空输出 | 1/200（本次） | 0；失败必须 typed error |
| 标题数偏差 | +45% | 绝对偏差不超过 10% |
| CLI 平均耗时 | 0.0382 s/doc | 回退不超过 10%，另报 p95 |
| 可复现性 | 缺 native artifacts | registry、lock、SHA、evaluation、latency 齐全 |

## 9. 最终判断

anydoc 的价值是把复杂但确定的 Office/ebook 源结构，经有安全预算、容错策略和统一 IR 的纯 Rust pipeline，稳定降维为 Markdown。package 安全层、canonical table grid 和统一 renderer 很值得 uparser 借鉴。它的主要限制是 IR 无页面几何、PDF 绕过 IR、无 OCR，以及不可再分发 benchmark 难以外部复现。

uparser native 是高性能 PDF 几何恢复引擎的产品化封装。它已完成去 liteparse、去 native PDFium、纯 Rust 内部化，也达到很强的无模型质量。但当前不能称为超过 pdf-inspector：Markdown 就是同源引擎直通，历史分数完全相同；uparser 自身增强还未体现在 Markdown benchmark。

短期最优策略是把 native 定位为电子 PDF 快速层，用 classification/text quality 精确分流，修复双轨 IR、标题过检和表格尾部；Office 文档复用 anydoc 这类源结构解析。这样 uparser 才能同时获得格式广度、native 速度和 VLM 对扫描/视觉布局的上限。

## 附录 A：关键源码

- anydoc API/PDF 特判：opensource/anydoc/src/lib.rs
- 格式分发/检测：opensource/anydoc/src/formats/mod.rs、detect.rs
- 统一 IR：opensource/anydoc/src/model
- 容器与预算：opensource/anydoc/src/package
- 格式前端：opensource/anydoc/src/formats
- GFM renderer：opensource/anydoc/src/render/markdown
- 绑定：opensource/anydoc/node、python、wasm
- native engine：uparser/crates/uparser-native-engine/src
- native adapter：uparser/crates/uparser-core/src/adapters/native.rs
- evaluator：benchmark/opendataloader-bench/src/evaluator*.py
- 历史评测：BENCHMARK_REPORT.md
- 内部化设计：NATIVE_ENGINE_INTERNALIZATION_DESIGN.md

## 附录 B：事实口径

- anydoc 4.4 ms/81 分来自其不可再分发 Office corpus 与 LLM judge，本文未独立复现。
- native 0.8754/0.9150/0.8141/0.7875 来自根报告；当前 checkout 无对应 evaluation.json。
- 本次只独立复跑当前 release binary 的 200 篇转换和结构计数，未重跑 Python evaluator。
- 不同硬件、进程模型和预热策略的速度仅用于量级判断，不用于精确倍数排名。
