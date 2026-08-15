# uparser Native ODF / EPUB 实施报告

> 更新日期：2026-08-15

## 1. 本轮交付

本轮在 `uparser-document-engine` 中新增了独立的 ODF 与 EPUB 格式前端，不依赖 anydoc，也不通过 LibreOffice、PDFium、OCR 或模型服务中转。

新增 native 支持矩阵：

| 格式 | 解析入口 | CanonicalDocument 单元 | native 协议名 |
|---|---|---|---|
| ODT | ODF `content.xml` | `Flow` | `native:odt` |
| ODP | ODF `content.xml` | 每个 `draw:page` 一个 `Slide` | `native:odp` |
| EPUB | container + OPF + XHTML | 每个 spine 项一个 `Chapter` | `native:epub` |

`--protocol auto`、显式 `--protocol native`、`--format document-json`、Rust API、Node `parseDocument` 和 Python `parse_document` 共用同一个格式引擎。

## 2. ODF 技术实现

ODT 与 ODP 共用 `formats/odf.rs`：

1. 使用受预算约束的 ZIP package reader 打开文档。
2. 检查 `META-INF/manifest.xml`，发现 `encryption-data` 时返回 typed `DocumentError::Encrypted`。
3. 读取唯一必需 part `content.xml`，以流式 XML event 方式解析，不构建完整 DOM。
4. ODT 将内容写入单一 `Flow`；ODP 按 `draw:page` 建立有序 `Slide`。
5. `text:h`、`text:p`、`text:list`、`text:a`、bookmark、换行及图片分别映射到统一 block/inline 模型。
6. `table:number-*-repeated` 在展开前计入 `max_expansion`；rowspan、colspan 映射为 origin/covered grid。
7. 包内 PNG、JPEG、GIF、SVG、WebP、BMP 使用 SHA-256 去重并生成稳定 asset ID。

## 3. EPUB 技术实现

EPUB 前端严格按出版物声明顺序读取：

```text
META-INF/container.xml
  -> rootfile / OPF
  -> manifest + metadata + spine
  -> spine 顺序的 XHTML chapter
  -> CanonicalDocument
```

已覆盖：

- Dublin Core 标题、作者、语言；
- spine 顺序与缺失 manifest 项 warning；
- XHTML `h1..h6`、段落、粗体、斜体、下划线、删除线、上下标和行内代码；
- 内外部链接、锚点、换行、图片；
- 有序/无序及嵌套列表；
- blockquote、pre/code block；
- HTML table 的 header row、rowspan、colspan 与 covered grid；
- 相对资源路径解析和防 package-root traversal；
- script/style 忽略，且不执行脚本、不加载网络资源。

## 4. 资源与安全边界

两个前端均受现有统一预算控制：输入大小、ZIP entry 数、单 entry 大小、声明解压总量、XML 节点数、XML 深度、结构展开量、图片大小及文本大小。EPUB/ODF 相对路径不能越过 package root；外部链接只保留为语义，不发起网络请求。

## 5. 当前不足

ODF 当前不足：

- 尚未解析 `styles.xml` 的完整样式级联，编号/项目符号主要保留列表结构，复杂 marker 样式可能退化为 bullet；
- ODF 注释、脚注、尾注只产生 unsupported-feature warning，尚未进入 canonical notes；
- ODP 尚未实现 master page、layout、animation、chart、公式及精确几何阅读顺序；
- ODF metadata、tracked changes、fields、目录和嵌入 OLE 对象尚未完整恢复；
- 图片 alt/caption 的 frame 级组合仍较基础。

EPUB 当前不足：

- 仅支持规范 XHTML/XML 内容，不容错解析任意 HTML；
- CSS 仅被安全忽略，未通过 computed style 推导视觉标题、隐藏元素、列表 marker 或分页；
- nav/NCX、landmarks、page-list、脚注语义、MathML、SVG 文本和媒体 overlay 尚未结构化；
- DRM/加密 EPUB 没有解密能力；
- 兼容 `ParseResult` 会损失 CanonicalDocument 的嵌套语义，完整结果应使用 document JSON API。

## 6. 验证结果

- `uparser-document-engine`：18/18 测试通过；
- `uparser-core --features native`：318 个库测试、29 个 CLI 测试、2 个合同测试通过；
- 跳过 2 个在 Windows 上硬编码 Unix `sleep` 的既有测试；
- document-engine、core native、uparser-napi 的 `clippy -- -D warnings` 通过；
- EPUB 适配器级测试确认输出 `native:epub`，并按 spine 章节生成兼容 page；
- ODF 测试覆盖加密拒绝、repeat 展开预算、图片、链接、嵌套列表及跨行跨列表格。

## 7. 后续优先级

1. ODF styles/metadata/notes 与 ODP master/layout。
2. EPUB nav/NCX、脚注、MathML 与 CSS 可见性规则。
3. RTF 前端。
4. legacy DOC/PPT 的 OLE 解析可行性评估。
5. 引入真实公开样本 golden corpus、损坏包和压缩炸弹回归集合。
