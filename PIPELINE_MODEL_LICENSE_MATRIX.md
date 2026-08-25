# Pipeline 模型资产与发布边界

本文件记录 `/home/dataset1/gaojing/magic-pdf.json` 所引用 pipeline 资产的工程发布边界，不构成法律意见。最终发布前仍需由许可证负责人确认模型卡、权重来源和依赖包条款。

| Stage | 配置模型 | 当前资产来源 | 初始发布策略 | 必需动作 |
|---|---|---|---|---|
| Layout | DocLayout-YOLO | PDF-Extract-Kit 模型目录 | `legacy-byom` | MinerU 已注明因 AGPLv3 移除；不得直接放入可分发制品 |
| Formula detection | YOLOv8-MFD | PDF-Extract-Kit 模型目录 | `legacy-byom` | MinerU 已注明因 AGPLv3 移除；不得直接放入可分发制品 |
| Formula recognition | UniMERNet-small | PDF-Extract-Kit 模型目录 | review required | 核对模型卡、权重和 tokenizer 的具体条款 |
| OCR | PaddleOCR PyTorch weights | PDF-Extract-Kit 模型目录与 `magic_pdf` runtime | review required | 分别核对 PaddleOCR 权重、转换代码和随包字典 |
| Table | RapidTable/SLANet-plus | `magic_pdf/resources/slanet_plus` | review required | 核对 RapidTable 包和 SLANet-plus 权重条款；记录权重 hash |
| Reading order | LayoutReader | 独立 layoutreader 目录 | `legacy-byom` | CC-BY-NC-SA-4.0；默认不进入商业可分发 profile |

## Profile 边界

### `legacy-byom`

- 用户或内部部署环境提供所有模型资产。
- uparser 只保存路径、hash 和模型 revision，不把权重复制进源码、安装包或镜像。
- 当前 `magic-pdf.json` 仅允许导入此 profile。

### `redistributable`

- 不包含 DocLayout-YOLO、YOLOv8-MFD 和 LayoutReader 权重。
- Layout/MFD 使用完成许可证核验的替代模型。
- 阅读顺序使用 `para_split` 或其他可分发实现。
- 每个模型必须在 manifest 中具有已审核的许可证标识和来源。

## 制品检查

发布检查必须分别覆盖：

1. 源码仓库。
2. Python wheel 和 Rust binary。
3. 容器镜像各层。
4. 自动下载脚本和默认 URL。
5. 示例配置及测试 fixture。

模型没有直接提交到 Git 并不代表可以由安装脚本自动下载；下载和缓存行为同样必须经过发布审查。
