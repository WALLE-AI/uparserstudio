# UParser V2 代码覆盖率分析报告

## 1. 测试结论

- 测试日期：2026-08-21
- 测试工具：`cargo llvm-cov 0.9.0`
- 测试范围：`uparser-core`、`uparser-document-engine`、`uparser-native-engine`
- 启用特性：`uparser-core/native`
- 测试结果：1243 个测试全部通过，0 个失败
- 整体行覆盖率：**86.43%**（53250 / 61613）
- 整体函数覆盖率：**88.76%**（4510 / 5081）
- 整体 Region 覆盖率：**86.78%**（90083 / 103810）

当前三个核心包的整体覆盖率较好，`uparser-core` 的覆盖最充分。主要风险集中在 Native Engine 的 PDF XObject、ToUnicode 和主入口异常分支，以及 Document Engine 的电子表格和旧版 DOC 解析路径。

## 2. 分包覆盖率

| 包 | 文件数 | 行覆盖率 | 函数覆盖率 | Region 覆盖率 |
|---|---:|---:|---:|---:|
| `uparser-core` | 43 | **91.08%**（9405 / 10326） | 89.26%（1080 / 1210） | 91.71%（13936 / 15195） |
| `uparser-document-engine` | 20 | **83.73%**（6988 / 8346） | 84.51%（540 / 639） | 81.80%（10316 / 12611） |
| `uparser-native-engine` | 33 | **85.83%**（36857 / 42941） | 89.42%（2890 / 3232） | 86.62%（65831 / 76004） |
| **整体** | **96** | **86.43%**（53250 / 61613） | **88.76%**（4510 / 5081） | **86.78%**（90083 / 103810） |

## 3. 主要覆盖缺口

以下文件按未覆盖行数和处理风险综合排序：

| 文件 | 行覆盖率 | 未覆盖行数 | 风险说明 |
|---|---:|---:|---|
| `uparser-native-engine/src/lib.rs` | 68.53% | 1456 | Native 主入口存在较多组合、错误和回退路径未覆盖 |
| `uparser-native-engine/src/tounicode.rs` | 53.70% | 1082 | PDF 字体编码、字符映射和异常 CMap 的质量风险较高 |
| `uparser-native-engine/src/extractor/xobjects.rs` | 6.80% | 480 | 图像、Form XObject 和嵌套资源解析测试明显不足 |
| `uparser-native-engine/src/detector.rs` | 82.71% | 404 | 文档特征检测的边界分支仍有缺口 |
| `uparser-native-engine/src/markdown/convert.rs` | 77.54% | 364 | 复杂布局转 Markdown 的组合场景覆盖不足 |
| `uparser-native-engine/src/extractor/fonts.rs` | 79.43% | 301 | 嵌入字体和异常字体字典仍需补测 |
| `uparser-document-engine/src/formats/sheet.rs` | 0.00% | 215 | 表格格式入口当前没有被测试执行 |
| `uparser-document-engine/src/formats/epub.rs` | 79.34% | 187 | EPUB 结构异常和资源缺失场景覆盖不足 |
| `uparser-document-engine/src/formats/doc.rs` | 58.19% | 171 | 旧版 DOC 解析与失败回退路径覆盖不足 |
| `uparser-core/src/cli.rs` | 77.83% | 145 | CLI 参数组合和错误输出分支尚未完全覆盖 |

## 4. 分析与判断

### uparser-core

行覆盖率达到 91.08%，说明统一 Runner、格式检测、路由、协议适配和 CLI 的主体流程已有较充分验证。后续重点不应是继续堆叠普通成功路径，而应补充取消、超时、无效配置、服务异常和路由冲突等分支。

### uparser-document-engine

总体行覆盖率为 83.73%，但不同格式之间分布不均。`sheet.rs` 为 0%，`doc.rs` 仅为 58.19%，意味着“支持某格式”与“该格式经过自动化验证”目前并不完全等价。应使用真实 XLS/XLSX/ODS/CSV 和旧版 DOC 样本覆盖识别、预处理、内容提取及失败降级链路。

### uparser-native-engine

总体行覆盖率为 85.83%，基础算法测试数量和覆盖面较好，但未覆盖代码集中于高复杂度模块。尤其是 XObject、字体编码和 ToUnicode，它们直接影响扫描件、复杂 PDF、工程规范和法律文书的文本保真度。因此，Native Engine 当前需要优先提升关键模块覆盖，而不是只追求整体百分比。

## 5. 建议补测优先级

1. **P0：PDF XObject 与 ToUnicode**  
   增加 Form/Image XObject、嵌套资源、循环引用、缺失资源、Identity-H/V、自定义 CMap、乱码和无 ToUnicode 字体样本。

2. **P0：电子表格格式**  
   覆盖 XLS、XLSX、ODS、CSV 的多 Sheet、合并单元格、公式、空行、隐藏 Sheet、日期与数字格式。

3. **P1：旧版 DOC 与异常文档**  
   增加损坏 OLE、密码保护、嵌入对象、复杂表格、编码异常和解析失败回退测试。

4. **P1：Native 主入口组合路径**  
   覆盖不同解析选项、分页范围、取消、超时、部分页失败和 Markdown 转换失败。

5. **P2：建立持续覆盖率门槛**  
   建议先以当前结果作为基线：整体行覆盖率不低于 85%，`uparser-core` 不低于 90%，另外两个引擎不低于 80%；同时禁止变更导致覆盖率下降超过 1 个百分点。

## 6. 统计限制

- 本报告是当前工作区代码的动态测试结果，不是对历史版本的趋势比较。
- 本次未启用 `pdfium` 等其他可选特性，因此条件编译后未进入构建的代码不在统计分母中。
- 远程模型协议主要通过测试替身验证；覆盖率不能替代真实 vLLM 服务和 OmniDocBench 的质量、吞吐量评测。
- LLVM 本次未生成有效的 Rust 分支覆盖数据，因此报告采用行、函数和 Region 覆盖率，不将 Branch 的 0 值解释为分支覆盖率为 0%。
- 仓库当前没有覆盖率 CI 门槛和历史报告，暂时无法判断覆盖率相较上一版本上升或下降。

## 7. 测试命令

```bash
cargo llvm-cov \
  --manifest-path uparser/Cargo.toml \
  --package uparser-core \
  --package uparser-document-engine \
  --package uparser-native-engine \
  --features uparser-core/native \
  --json
```
