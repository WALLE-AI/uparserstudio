# 智能文本解析范式分类报告：端到端模型 vs 传统 Pipeline

> 基于 `chandra_report.md`/`dots_ocr_report.md`/`liteparse_report.md`/`mineru_report.md`/`monkeyocr_report.md` 中"使用的模型清单"章节的分析结果，对 `opensource/` 五个项目所采用的技术路径做归类。

## 1. 端到端模型（单一生成式 VLM 一次性完成版面理解+内容识别）

| 项目/backend | 模型 | 特点 |
|---|---|---|
| **chandra** | `datalab-to/chandra-ocr-2` | 唯一的模型。图像 → 带 `data-bbox`/`data-label` 的 HTML，一次（或少量）推理内完成版面分类、OCR、表格、公式全部工作 |
| **dots.ocr** | `rednote-hilab/dots.mocr` | 唯一的模型。整页一次 VLM 调用同时给出版面检测+分类+阅读顺序+内容识别（表格→HTML、公式→LaTeX），是"真正一次性"的端到端方案 |
| **MinerU `vlm` backend** | `opendatalab/MinerU2.5-Pro-2605-1.2B` | 唯一的 VLM。但内部是"①整页版面检测 → ②逐区块内容识别"两阶段 prompt 结构——**模型只有一个，但请求分两步**，与 dots.ocr/chandra 的"单次调用"型端到端略有不同 |

这三者的共同点：**不引入独立的 CV 版面检测模型/OCR模型/表格模型/公式模型**，需要部署的模型只有一个 VLM，因此更容易通过 vLLM 等方式外部化服务。

## 2. 传统 Pipeline（多个专用小模型串联/并联）

| 项目/backend | 组成模型 | 特点 |
|---|---|---|
| **MinerU `pipeline` backend** | PP-DocLayoutV2（版面检测）→ PaddleOCR-torch（OCR检测+识别，遮罩行内公式）→ Unimernet/pp_formulanet（公式识别 MFR）→ PP-LCNet（表格有线/无线分类）→ SLANet-Plus/Unet（表格结构识别） | 最经典的多段流水线。每一段都是独立的专用小模型，完全不使用生成式 VLM。显存常驻成本高，但不依赖外部 VLM 服务 |
| **MonkeyOCR**（Structure-Recognition-Relation 三段式） | PP-DocLayoutV2/DocLayout_YOLO（Structure：版面检测）+ MonkeyOCR-pro-VLM（Recognition：区域内容识别）+ LayoutLMv3系 layoutreader（Relation：阅读顺序） | **混合定位**——版面检测与阅读顺序是传统判别式模型，只有内容识别环节用 VLM，三段式结构，不是纯端到端 |
| **liteparse**（本体） | 不使用模型（PDFium 原生文本层抽取 + 空间网格投影算法）；仅在文字层稀疏区域可选调用外部 OCR 引擎（内置 Tesseract，或参考实现 EasyOCR/PaddleOCR/SuryaOCR） | **既非 VLM 端到端也非典型多模型 Pipeline 的第三条路**——主体是非 ML 算法，OCR 只是辅助性、局部性介入；EasyOCR/PaddleOCR 本身是传统的"检测+识别"两段式 CV 流水线 |

## 3. 补充：MinerU `hybrid` backend 的定位

`pipeline`（版面/OCR检测的低成本定位）+ `vlm`（内容识别）组合而成的**第三类**，兼顾传统流水线的定位精度与端到端 VLM 的识别质量，既不是纯端到端，也不是纯传统 Pipeline，而是二者的折衷形态。

## 4. 结论

| 归类 | 涵盖对象 |
|---|---|
| 纯端到端 | chandra、dots.ocr、MinerU-vlm |
| 纯传统 Pipeline | MinerU-pipeline（+ liteparse 的外接 OCR 部分） |
| 混合 | MonkeyOCR、MinerU-hybrid |

该分类与此前 `ARCHITECTURE.md` v0.3 §0 中"方案①端到端 / ②传统流水线 / ③混合"的划分一致，可作为后续 uparserstudio core 设计 Protocol Adapter（`ARCHITECTURE.md` v0.4）时判断各模型协议应归入哪种预处理/后处理策略族的依据。
