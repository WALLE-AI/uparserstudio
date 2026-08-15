# uparser Native RTF 实施报告

> 更新日期：2026-08-15

## 已交付

新增 `uparser-document-engine/src/formats/rtf.rs`，采用受预算约束的字节扫描器和 group state stack，不依赖 anydoc、正则替换、LibreOffice 或模型服务。

已覆盖：

- RTF header 与内容检测；
- ANSI codepage 1250/1251/1252、Shift-JIS 932；
- `\'hh` byte run 和 `\uN`/`\ucN` Unicode fallback，包括 UTF-16 surrogate pair；
- bold、italic、underline、strike、super/subscript；
- paragraph、line break、page break、outline heading；
- list、table row/cell；
- HYPERLINK field、internal anchor、bookmark；
- footnote/endnote reference 与 canonical note body；
- pict hex 与 `\binN` 图片，SHA-256 asset 去重；
- suppressed/ignorable destination；
- group depth、token、decoded text、binary 和 picture size 预算。

RTF 已接入显式 native、auto 路由、Markdown、document-json、Rust API、Node/Python 共用入口，兼容协议名为 `native:rtf`。

## 验证

- document-engine 当前 21/21 测试通过；
- RTF 专项覆盖多编码、Unicode、样式、field、note、bookmark、list、table、pict 和资源限制；
- `uparser-core` adapter 测试确认 `native:rtf` 和 logical page lowering；
- document-engine `clippy -- -D warnings` 通过。

## 尚存限制

- 尚未解析 font table 对字体级 codepage 的覆盖；
- listtable/listoverride 的复杂 marker/start/nesting 目前降级为结构化 decimal list；
- nested table、cell merge、完整 stylesheet cascade 尚未实现；
- field 仅结构化常见 HYPERLINK，其他 field 保留可见结果；
- picture crop/size/alt、OLE object、drawing shape 尚未恢复；
- 需要继续增加真实 RTF corpus、mutation 和 fuzz target。
