# `uparser` CLI 模块完善方案

> 状态：设计草案，未实施。基于本次会话中对 `crates/uparser-core/src/cli.rs`（816 行）及相关模块的真实使用/调试经验撰写，不是泛泛的 checklist。

## 0. 背景与动机

CLI 模块（`cli.rs` + 其调用的 `api.rs`/`scheduler.rs`/`cache.rs`）在 P0–P10 的迭代中已经具备了 `parse`/`classify`/`cache`/`doctor`/`protocols` 五个子命令、六种协议、缓存、流式输出、Node/Python 绑定。功能完整度较高，但本次用真实 vLLM 端点（`MinerU2.5-2604-1.2B`）跑真实文档（7 页、107 页两份）时，暴露出几类**结构性问题**：

1. **长任务完全不可观测**：解析 107 页文档时，`stderr` 在整个过程中没有任何输出，唯一的信号是进程最终退出。之前诊断那次真实死锁 bug 时，也是因为没有任何进度信号，不得不用 `ps`/`ss`/`nvidia-smi` 从操作系统层面排查，才定位到是 `scheduler.rs` 的信号量嵌套获取导致的死锁（已修复，见 `CLAUDE.md` "Post-P10" 记录）。**这类问题不该靠外部工具排查，CLI 自己应该能说清楚"卡在哪一步"。**
2. **命令行参数已经很长**：真实调用一次 `mineru-vlm` 需要 6+ 个 flag（`--protocol --endpoint --model --max-concurrency --format ...`），每次手动输入容易出错（本次会话里就出现过一次输出路径写错的情况）。
3. **`cli.rs` 与 `api.rs` 存在文档化但未消除的重复逻辑**（P10 遗留）。
4. **`postprocess.rs`（文档级段落合并）从建成到现在从未被真实调用链调用过**——`cli.rs`/`api.rs`/`scheduler.rs` 里完全没有调用点，所有真实解析结果都是协议原始输出，未经任何文档级后处理。这是审计后处理模块时发现的最大问题，详见下方专门一节。
5. **`robustness.rs`（退化输出检测 + 温度升级重试）自 P1 建成后同样从未被任何 adapter 接入**——和 postprocess.rs 一样"设计了但完全没用上"，而这恰恰是真实模型调用最需要的能力：VLM 在某些区块上确实会输出退化内容（本次两份真实文档都没触发，但这只是运气好）。
6. **VLM 输出的文本内容本身有质量问题，且没有任何规范化步骤**：本次真实输出里发现同一文档相邻列表项标点全半角不统一（`;` vs `；`）的具体案例。
7. **Markdown 渲染过于朴素**：`render::to_markdown` 只是把每个 block 的 text/html/latex 顺序拼接，标题不分级、列表没有 `-`/编号、页码 block（`page_number`）会原样混进正文——用户看过一次真实输出后已经注意到这一点。
8. **大文档缺少"先试跑一部分"的能力**：107 页文档如果只是想验证协议/端点是否work，目前只能全量跑完才知道结果。
9. **对全部源码做了一轮四路并行深度审阅之后，又发现两个和 #4/#5 同类的"整段能力从未接入"问题**：`ingest::ingest_document`（非 PDF/图片格式的正确处理路径）从未被 `cli.rs`/`api.rs` 调用，非 PDF/图片输入会静默退化成一张 1×1 占位图后送进模型，**exit 0、无报错、结果完全错误**；`ParseResult.capability_notes`/`.warnings` 两个字段从声明起从未被写入过，adapter 里大量真实的降级/修复警告只打到 stderr，通过 API/绑定层调用的调用方完全看不到。另外还审出 30+ 项分布在并发管理、网络重试、VLM 输出内容正确性、阅读顺序算法、CLI 参数一致性等方面的具体问题，详见"深度源码审阅"一节。

本方案按**优先级**排列，每一项给出问题、设计、影响范围、测试要点。P0 级是这次真实使用直接暴露、以及深度源码审阅发现的最高杠杆项；P1/P2 是结构性但不紧急的项目。

---

## P0：长任务可观测性（Progress Reporting）✅ 已完成（见 `CLAUDE.md` "Post-P10 continued" 第三批实现记录）

> 实现与设计略有差异：没有单独的 `ProgressSink` trait，而是 `Scheduler::run_with_progress<F>(..., on_page: F) where F: FnMut(&PageProgress)` 回调（`PageProgress { page_num, ok, completed, total }`），`run()` 内部委托给它并传入空回调，保持向后兼容。`cli.rs::run_parse` 用这个回调实现了节流打印（≥900ms 间隔）+ 后台看门狗任务（每 5s 检查一次，超过 30s 无页面完成即打印 stall 警告）。已在 7 页和 107 页真实文档上用真实 vLLM 端点验证过节流输出效果。

### 问题

`scheduler.rs::run`/`run_streaming` 内部按 window 处理页面，但只有等一个 window **全部完成**才有任何信号（`run_streaming` 才有回调，`run` 完全没有）。对一个 107 页、`window_size=16` 的文档，意味着至少要等 ~16 页全部完成（网络请求 + 模型推理）才会有第一次"活着"的信号。如果某一页卡住（无论是本次遇到的死锁类 bug，还是未来的网络抖动/模型服务假死），用户拿到的信号和"进程死了"是完全一样的——都是沉默。

### 设计

在 `Scheduler` 上加一个进度回调接口，`run`/`run_streaming` 内部在**每个 window 内的每一页完成时**（不必等整个 window）都触发一次，而不是现在 `run_streaming` 那种"整个 window 完成才回调一次"的粒度：

```rust
pub struct ProgressEvent {
    pub completed_pages: usize,
    pub total_pages: usize,
    pub last_page_num: u32,
    pub last_page_ok: bool,
    pub elapsed: Duration,
}

pub trait ProgressSink: Send + Sync {
    fn on_page_done(&self, event: ProgressEvent);
}
```

`cli.rs` 默认实现一个 `StderrProgressSink`，按固定节流（比如每完成一页或每 2 秒，取较慢者）往 `stderr` 打一行：

```
progress: 42/107 pages (page 41 ok, 39.3s elapsed, ~24s remaining)
```

**关键的第二部分**：加一个**心跳/看门狗**机制——如果超过一个阈值（默认 30s，可配 `--stall-warning-secs`）没有任何页面完成，主动打印一行警告到 stderr：

```
warning: no page has completed in 32s (4/16 permits in use, last completion: page 3 at 12.1s) — this may indicate a stalled request or a scheduling deadlock
```

这个心跳信息应该包含**信号量当前占用情况**（`Semaphore::available_permits()`），因为死锁 bug 的核心症状就是"permit 全部被占用但没有任何请求在真正跑"——如果当时 CLI 自己就能打印"4/4 permits in use, 0 active network connections"，本次诊断死锁会从"看 `ps`/`ss` 十几分钟"缩短到"看一行 stderr 秒懂"。这不是过度设计，是这次真实调试直接证明了需求的功能。

### 影响范围

- `scheduler.rs`：`Scheduler::run`/`run_streaming` 增加 `progress: Option<Arc<dyn ProgressSink>>` 参数（用 `Option` 保持向后兼容，`None` 时行为不变，现有 200+ 测试不受影响）。
- `cli.rs`：`run_parse` 默认传入 `StderrProgressSink`；新增 `--quiet` 抑制进度输出（见 P1 日志分级）。
- `api.rs`：暴露 `ParseOptions::progress: Option<Arc<dyn ProgressSink>>`，绑定层（napi/python）可选择性接入（比如 Node 侧转成一个 EventEmitter）——但这个不是本方案必须项，可以留到真的有绑定层用户需求时再做。

### 测试要点

- 单元测试：给 `Scheduler::run` 一个慢 mock adapter，断言 `ProgressSink` 收到的 `completed_pages` 单调递增、`total_pages` 恒定、最后一次等于总页数。
- 看门狗测试：mock 一个"卡住不返回"的 adapter（`tokio::time::sleep(Duration::MAX)` 或永久 pending 的 future），断言在心跳阈值后 `StderrProgressSink` 输出了 stall 警告（可用 `tokio::time::pause()`/`advance()` 控制虚拟时间，不必真等 30s）。

---

## P0：`--pages` 范围选择 ✅ 已完成（见 `CLAUDE.md` "Post-P10 continued" 第三批实现记录）

> 实现与设计一致：新建 `page_range.rs`（`parse_page_range`，10 个单测覆盖单页/区间/逗号组合/非法输入），`cli.rs`/`api.rs` 均在调用 `scheduler.run` **之前**过滤页面。

### 问题

大文档想先验证协议/端点/prompt 是否 work，目前只能整份跑。107 页文档哪怕只想看第 50 页有没有问题，也得等前 49 页跑完（`run` 非流式模式）或者自己写脚本切 PDF。

### 设计

`parse` 子命令新增：

```
--pages <RANGE>   # e.g. "1-5", "3", "1,5,10-12"；省略则解析全部页
```

实现层面：`ingest::rasterize`/`rasterize_or_fallback` 返回全部 `RenderedPage` 后，在 `api.rs::parse`/`cli.rs::run_parse` 里按 `--pages` 过滤，**在调用 `scheduler.run` 之前**过滤，而不是解析完再丢弃（否则起不到"少跑省钱省时间"的作用）。

页码解析用一个简单的 `parse_page_range(&str) -> Result<Vec<u32>, String>` 纯函数，放在 `cli.rs` 或新建 `page_range.rs`（更好，方便单测 + 未来 `native`/绑定层复用）。

### 测试要点

- `page_range.rs` 纯函数单测：单页、区间、逗号组合、非法输入（`"abc"`、`"5-2"` 倒序、越界）各给出清晰错误而不是 panic。
- CLI 集成测试：`--pages 2-3` 跑 mock 协议的多页假文档，断言 `result.pages.len() == 2` 且 `page_num` 恰好是 2、3。

---

## P0：后处理模块强化（VLM 内容后处理）

> 这一节是本次专门审计"后处理模块现状"后新增的——发现的问题比预想的更基础：**`postprocess.rs` 从建成到现在，从未被真实调用链调用过一次。**

### 0. 现状确认：`postprocess.rs` 是一个"设计完整、测试齐全、但从未上线"的模块

逐一确认过 `cli.rs`、`api.rs`、`scheduler.rs` 三个真实调用路径：

```bash
grep -rn "postprocess::" src/cli.rs src/api.rs src/scheduler.rs
# (无输出)
```

`postprocess::merge_paragraphs_by_geometry`（纯几何段落合并，P1 建成，Gate G2 验证过对 mineru-vlm/dots.ocr 两种协议形状都能正确工作）**目前只在它自己的单元测试里被调用过**。这意味着：不管走哪个协议、哪次真实解析，`scheduler.run()` 返回的 `Vec<Page>` 都是**未经任何文档级后处理的原始 block 列表**，直接进了 `render::to_markdown`/`to_json`。本次两份真实文档（7 页、107 页）的输出，严格来说都是"协议原始输出"，不是"完整后处理产物"——只是因为 mineru-vlm 的版面检测本身按段落分块（不是按行），所以碰巧看起来还可以，但这不能依赖运气。

这是目前后处理链条里**优先级最高**的问题：不是"哪个环节做得不够好"，而是"整个环节都没有被启用"。

### 1. 问题清单（按影响面排序）

1. **`postprocess.rs` 未接入真实调用链**（见上）——文档级段落合并完全没有生效。
2. **信号增强层没有任何真实输入**：`ProtocolAdapter::emitted_signals()` 在全部六个模型类协议（`mineru-vlm`/`dots-ocr`/`monkeyocr-v2`/`pipeline`/`paddleocr`/`mock`）上都返回 `PostprocessSignals::default()`（`spans`/`merge_hint`/`font_size` 全 `false`）——只有 `native`（零模型、直接读 PDF 文本层）是唯一真正提供这些信号的 adapter。也就是说，`postprocess.rs` 设计时预留的"信号驱动"分支，对所有真实模型调用来说**永远走不到**，只能靠纯几何规则。这不是这次要解决的问题（重新做模型侧信号提取工作量很大且部分模型本身不吐这些信号），但必须在文档里如实标注，不能假装"后处理已经是信号增强级别"。
3. **标题分级未实现**：`ARCHITECTURE.md` §14 已经把它从"共享能力"下调为"能力门控的可选增强"（`--title-leveling-endpoint`，需要外部 LLM），目前这个 flag 和对应逻辑都不存在，所有 `title` 类别 block 一律扁平。
4. **跨页合并未实现**（段落 + 表格）：`scheduler.rs` 按页独立处理，`postprocess.rs` 现有函数签名虽然接受 `Vec<Block>`（不限定页内），但实际调用点（不存在）不会跨页拼接——一段话如果正好卡在页尾，会被硬切成两个 block，渲染成两段。跨页表格（表格贯穿页面边界）目前也不合并。
5. **VLM 输出的文本层面质量问题，目前没有任何规范化步骤**——这是本次真实输出里发现的具体案例，不是猜测：

   ```
   (二)安全投入符合安全生产要求;      <- 半角分号
   (三)设置安全生产管理机构...；      <- 全角分号（其余各条目均为此）
   ```
   
   同一份文档、同一个 `list` 容器下的相邻条目，模型输出的标点全半角不统一。这是 VLM 生成文本的常见特征（训练数据里两种标点都出现过），`output_parse.rs`/`otsl.rs`/`formula_repair.rs` 目前都只处理各自协议的**结构层**解析，没有任何模块负责这种**文本层**规范化。

### 2. 设计方案

#### 2.1 把 `postprocess.rs` 真正接入调用链（最优先，风险最低）

在 `api.rs::parse` 里，`scheduler.run()` 返回 `result_pages` 之后、构造 `ParseResult` 之前，加一步：

```rust
let result_pages: Vec<Page> = result_pages
    .into_iter()
    .map(|page| Page {
        blocks: postprocess::merge_paragraphs_by_geometry(page.blocks),
        ..page
    })
    .collect();
```

`cli.rs::run_parse` 同步加（保持和 `api.rs` 一致的"已知重复"现状，不在这次顺带做统一，避免范围蔓延）。新增 `--no-postprocess` flag 跳过这一步，方便调试时对比"协议原始输出 vs 后处理后输出"的差异——这本身也是排查"后处理是否引入了错误合并"的必要工具。

**注意**：这一步只做 per-page 合并（`merge_paragraphs_by_geometry` 目前的语义就是页内 block 列表），**不是**跨页合并——2.3 节单独设计。先接入这一步已经能让"文档级后处理"这五个字第一次对真实解析生效，且改动小、风险低、Gate G2 已经验证过正确性。

#### 2.2 VLM 输出文本规范化（`content_normalize.rs`，新模块）✅ 已完成（见 `CLAUDE.md` "Post-P10 continued" 第十一批实现记录）

> 实现与设计基本一致，一处加强：`.` 的半角转全角额外加了"前后都是 ASCII 数字则不转换"的保护（避免把 `3.14` 这种小数点误判成中文句号），这是设计草案没提到但真实存在的风险点。调用点也按设计放在 `postprocess::merge_paragraphs_by_geometry` 循环内、合并判断之前，对每个 `text` 非空的 block 生效（不限定 category=="text"）。已用真实案例在真实 vLLM 端点上验证：`（二）安全投入符合安全生产要求；`/`（三）设置安全生产管理机构...；` 两个相邻列表项现在都是全角分号，之前是半角/全角混用。

新增一个纯文本处理模块，专门处理"模型生成文本"层面的常见问题（不涉及版面/结构，只处理 `Block.text`/`Block.html` 里的字符串内容）：

```rust
/// 半角/全角标点统一：当一段文本以中文字符为主（CJK 字符占比超过阈值，
/// 避免误伤代码块/纯英文内容/公式）时，把常见的半角标点
/// （, . ; : ? ! ( )）统一成对应全角形式，与文本中已有的全角标点风格保持一致。
pub fn normalize_punctuation(text: &str) -> String { ... }

/// 折叠连续空白（模型偶尔会在文本中插入多余空格），保留单个换行/单个空格。
pub fn collapse_whitespace(text: &str) -> String { ... }
```

调用点：`postprocess.rs` 的合并流程里，对每个 `text` 非空的 block，在合并**之前**先跑一遍 `content_normalize`（合并逻辑依赖文本内容做判断的地方不多，顺序不敏感，但先规范化能让后续基于标点的段落边界判断——如果未来要做——更可靠）。

**范围控制**：`normalize_punctuation` 必须做"是否以中文为主"的判断再决定要不要转换，否则会把英文文档、代码块、数学公式里合法的半角标点也改掉——这是这个功能最大的风险点，测试要重点覆盖"混合中英文文本不应该被误伤"的场景。

#### 2.3 跨页段落合并（新函数，独立于 2.1 的页内合并）

```rust
/// 检测页尾最后一个 text block 是否"看起来没说完"（不以句末标点结尾），
/// 如果是，且下一页第一个 block 也是 text 类别、几何上在页首附近，
/// 则拼接成一个跨页 block（`geom_frame` 保留原页信息，新增
/// `CoordFrame` 变体或在 `Block` 上记录"spans multiple pages"——需要
/// 先确认 IR 是否要为此扩展，这是本节唯一涉及 IR 改动的部分）。
pub fn merge_paragraphs_across_pages(pages: Vec<Page>) -> Vec<Page> { ... }
```

这是这次方案里**唯一可能需要动 IR（`types.rs`）** 的部分——如果要如实标注"这个 block 的内容横跨了第 N/N+1 页"，`Block` 目前没有承载"跨页"信息的字段。两个选项：

- **选项 A（简单）**：合并后的 block 归属到起始页，`bbox_px`/`geom` 保留起始页坐标，不新增字段——代价是丢失"这段话其实横跨两页"的可追溯性。
- **选项 B（完整）**：给 `Block` 加 `spans_pages: Option<(u32, u32)>` 字段——改动 IR，所有 `Block` 构造点都要过一遍（六个 adapter + postprocess 自己），影响面较大。

建议先做**选项 A**，跨页合并本身先作为独立于 2.1 的可选步骤（`--merge-cross-page`，默认关闭），观察真实文档上的合并质量后再决定要不要为可追溯性升级到选项 B。

#### 2.4 表格跨页合并、标题分级——本轮不做

- 表格跨页合并：判断逻辑比段落复杂得多（需要比较表头是否重复出现、列数是否一致），且本次两份真实文档都没有跨页表格可供验证，建议先记录为已知限制，等有真实跨页表格样本时再设计。
- 标题分级：`ARCHITECTURE.md` §14 已经明确这是"需要外部 LLM 端点"的能力门控增强，不是这次"强化现有后处理模块"范围内能低成本做的事，维持现状（扁平）。

### 3. 测试要点

- **接入验证**（2.1）：一个新的 CLI 集成测试或 `api.rs` 单测——构造两个几何上应该合并的 mock text block，跑完整 `api::parse` 流程（不是直接调用 `postprocess::merge_paragraphs_by_geometry`），断言最终 `ParseResult` 里确实只剩一个合并后的 block；再跑一次 `--no-postprocess`，断言还是两个。这是证明"从建成到现在从未被调用"这个问题被修好的唯一方式——必须走真实调用链，不能只测函数本身（函数本身的测试已经存在且一直是绿的，这恰恰是问题被忽视这么久的原因之一）。
- **`content_normalize.rs`**：直接用本次真实案例做单测——`"安全投入符合安全生产要求;"` 规范化后应该变成 `"安全投入符合安全生产要求；"`；再加一个反例，纯英文/代码内容里的半角标点不应该被改动。
- **跨页合并**：构造一个"页尾不完整句子 + 页首延续"的 mock 双页数据，断言合并后是一个 block；再构造一个"页尾本来就是完整句子"的场景，断言不会被错误合并（避免过度合并把两个独立段落粘在一起）。

---

## 深度源码审阅：跨模块问题清单

> 方法说明：对全部 ~25 个源文件做了 4 路并行的聚焦代码审阅（协议适配器 / 共享处理模块 / 基础设施 / CLI-API-类型系统），每路要求给出精确到文件:行号的具体问题，而不是泛泛建议。共发现 35+ 项，去重后按影响面归类如下。**两个独立审阅路径分别从不同角度独立发现了同一个 `geometry.rs::rescale_bbox_to_original` 除零问题**（见 D.1），交叉验证提高了这类发现的可信度。

### A. 最高优先级：三个"整段能力从未生效"的发现

这三项和前面已经确认的 `postprocess.rs` 从未被调用属于同一类问题——**不是代码写错，而是写对的代码从没被接进真实调用链**，本次审阅又找到三个：

1. **`ingest::ingest_document`（§13.1a 的 `detect_format → structured_bypass → normalize_format` 完整流水线）从未被 `cli.rs`/`api.rs` 调用**——两处的 `rasterize_or_fallback` 只会尝试 pdfium 光栅化或 `image::load_from_memory`，从不调用 `ingest_document`/`structured_bypass`/`normalize_format`。真实后果：一个 `.docx`/`.pptx`（需要 LibreOffice 转换）或 `.xlsx`/`.csv`（需要走 `structured_bypass` 免模型直读）喂给 `uparser parse`，会在 `image::load_from_memory` 失败后跌到 1×1 占位图，然后被送进随便哪个协议 adapter 当作"一张空白图片"处理——**exit 0，没有任何报错，输出静默错误**。这比 postprocess 未接入还严重，因为 postprocess 未接入只是"质量打折"，这个是"结果完全错误但看起来正常退出"。
2. **`ParseResult.capability_notes`/`.warnings` 字段从声明到现在从未被任何代码路径写入过一次**——全部构造点都是 `vec![]`。同时，`adapters/*.rs` 里大量真实存在的 `eprintln!` 警告（分类回退、坐标越界、修复了 N 处缺失分隔符……）只打到 stderr，从未汇入这两个字段。这意味着通过 `api.rs`/Node/Python 绑定调用（不是交互式终端、看不到 stderr）的调用方，**完全接收不到任何"这次解析发生了降级/修复"的信号**——JSON 输出看起来干干净净，实际上可能已经丢了内容。两个独立审阅路径都各自发现了这个问题（一个从 `types.rs` 角度、一个从 adapter 的 `eprintln!` 角度），交叉确认。
3. （已在"后处理模块强化"一节详述，此处仅重复列出以保持"从未接入"类问题的完整性）`postprocess.rs` 从未被调用。

**这三项建议合并成一次"接线"迭代**：核心工作量都是"在 `api.rs::parse`/`cli.rs::run_parse` 里补上早就写好的调用"，不是新写复杂逻辑，性价比最高、风险最低，应该排在整个方案的最前面。

### B. 并发/资源管理（延续本次死锁修复的同一类风险）

| # | 位置 | 问题 | 影响 |
|---|---|---|---|
| B.1 ✅ | `scheduler.rs::run`（`handle.await.expect(...)`） | 单页 adapter 内部一旦 panic（比如索引一个恶意/畸形模型响应越界），会通过 `expect` 重新 panic，直接打断整个 `run`/`run_streaming`，**丢失同一批次里所有已经跑完的其它页面结果**——和模块自己声称的"per-page failure isolation"矛盾，目前只隔离了 `Err`，没隔离 panic | 一份大文档只要有一页触发 panic，整份文档的解析结果全部丢失，而不是那一页报错、其它页正常返回 —— **已修复**：`JoinError` 转成该页的 `PageError`，新增回归测试 `panic_on_one_page_is_isolated_not_fatal_to_the_whole_run` |
| B.2 ✅ | `mineru_vlm.rs`/`monkeyocr_v2.rs`/`pipeline.rs` 的 stage-2 循环 | `ctx.acquire_permit()` 拿到 permit 之后，还在**持有 permit 期间**同步做裁剪/旋转/缩放/base64 编码（CPU 密集、且没用 `spawn_blocking`，会占住 tokio 工作线程） | 这些 CPU 工作被算进了本该只衡量"网络请求并发"的信号量预算里，实际网络并发会低于 `--max-concurrency` 配置值，且可能让 tokio 工作线程被阻塞，间接拖慢其它任务 —— **已修复**：permit 挪到紧贴各自的 `dispatch()`/`dispatch_rest()` 调用前；真实端点回归耗时从 ~21s 降到 12.2s（未做严格 benchmark，但方向符合预期）。`spawn_blocking` 包裹 CPU 工作本身未做，判断收益/风险比不足以在这轮做 |
| B.3 | 同上三个文件 | stage-2 的"逐 block 编排"逻辑（enumerate → 判断跳过 → 拿 permit → 裁剪缩放编码 → dispatch → 按 index 收集结果）在三个文件里**几乎逐字复制了三遍**（各 ~35 行） | 这正是这次死锁 bug 产生的根源模式——同一段逻辑散在多处，改一处漏改另一处的风险始终存在；建议抽成共享的 `fan_out_stage2` 辅助函数 —— **本轮刻意未做**：三个 adapter 的 pending-item 类型和逐项闭包结构并不完全一样，抽象成通用共享函数是有真实行为漂移风险的重构，判断风险高于本轮收益，留作独立的、有明确跟踪的后续项 |
| B.4 ✅ | `ingest.rs::convert_via_libreoffice`/`convert_via_imagemagick` | 子进程超时后，`tokio::time::timeout` 只是放弃了等待这个 future，**没有 `kill_on_drop(true)`，真实的 `soffice`/`magick` 进程仍在后台继续跑**，长期批量转换会积累僵尸/孤儿进程 | 高负载批处理场景下会逐渐耗尽进程数/文件描述符/内存，且完全不可见（表面上看只是"某次转换超时了"） —— **已修复**：`run_with_timeout` 内统一 `kill_on_drop(true)`，新测试 `kill_on_drop_actually_terminates_the_child_on_timeout` 真实验证过（临时去掉修复确认测试会失败） |
| B.5 ✅ | `ingest.rs::convert_via_libreoffice` | 没有传 `-env:UserInstallation=file://<独立临时目录>`，headless LibreOffice 靠用户 profile 锁串行化，并发跑第二个 DOCX/PPTX 转换大概率卡死或失败，误判成 `ConversionTimedOut` | 一旦真的接了并发批处理场景，这个问题几乎必现 —— **已修复**：每次调用传独立的 `-env:UserInstallation`（复用同一份调用已有的独立临时目录） |
| B.6 | `scheduler.rs` 模块级文档注释 | 声称"窗口把峰值内存限制在 O(窗口大小) 而不是 O(总页数)"，但 `ingest::rasterize()` 其实是一次性把**全部页面**光栅化成 `Vec<RenderedPage>` 再传给 `scheduler.run`，窗口只限制了"同时处理的页数"，完全没限制内存 | 对高 DPI、几百页的大文档，实际内存占用和文档注释描述的完全不符，这是一个真实的 OOM 风险点，不只是文档不准确 |
| B.7 | `transport.rs::Transport::new()` | 默认并发是 `Semaphore::MAX_PERMITS`（近乎无限），只有显式调 `with_concurrency` 才会真正限流；测试代码和潜在的未来"快速路径"都可能直接用 `new()` | 和模块自身"文档级并发预算"的设计初衷相反 |
| B.8 | `cache.rs::put` | 直接 `std::fs::write` 到最终路径，非原子——两个并发 `uparser parse` 处理同一份文档会竞争写同一个缓存文件 | `get` 会把损坏 JSON 当成 miss 处理，所以不会崩，但会导致并发场景下反复缓存未命中、重复解析，而不是干净地由一方获胜 |

### C. 网络重试/超时鲁棒性

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| C.1 | `transport.rs::post_with_retry` ✅ | 只对 5xx 重试，**429（限流）完全不重试**，也不看 `Retry-After` | 429 是真实 LLM/OCR 后端负载下最常见的失败模式，必须单独处理（实际实现：429 与 5xx 同等重试；新增 `retry_after_from_headers` 解析数字秒形式的 `Retry-After`（HTTP-date 形式回退到计算退避），并加了 30s 兜底上限防止后端返回异常大值） |
| C.2 | 同上 ✅ | 退避 `50ms * 2^n` **无上限、无抖动**，第 10 次重试就要睡 ~25.6s，且所有并发失败任务按相同确定性节奏退避，容易在后端恢复瞬间集中撞车 | 加封顶 + 抖动（decorrelated jitter）（实际实现：`jittered_backoff` 用 AWS "full jitter" 策略——`[0, min(cap, base))` 内随机，cap=5s；未引入 `rand` crate，用墙钟纳秒作为廉价熵源，仅用于调度抖动非安全敏感场景） |
| C.3 | 同上 ✅ | 200 响应但 body 不是合法 JSON（网关截断/返回错误页但状态码仍是 200）**直接失败不重试** | 和网络错误一样应该计入重试预算（实际实现：`resp.json()` 解析失败时不再直接 `?` 冒泡，而是计入同一个重试循环，指数退避+抖动后重新发起完整请求） |
| C.4 | `adapters/mod.rs::ParseCtx::dispatch_rest` ✅ | 硬编码 `timeout: 60s, max_retries: 2`，完全无视 `PipelineAdapter`/`PaddleOcrAdapter` 结构体上公开的 `timeout`/`max_retries` 字段——**这两个字段被声明了但从未被读取过** | 未来任何"给 pipeline/paddleocr 配置更长超时"的尝试都会看似生效、实际被忽略；应该把这两个字段真正传进 `dispatch_rest`（实际实现：`dispatch_rest` 签名新增 `timeout`/`max_retries` 参数，`pipeline.rs`（4 处调用点）/`paddleocr.rs`（1 处）全部改为传入 `self.timeout`/`self.max_retries`；新增测试直接证明短 timeout 真的被 `Transport` 采用而不是被忽略） |
| C.5 | `transport.rs` ✅ | 单次调用最坏情况耗时 = `timeout × (max_retries+1) + Σ退避`，**没有外层总耗时上限**，一个大 `timeout` + 大 `max_retries` 组合可以让一页请求无限期拖住整个批次 | 加一层外层 `tokio::time::timeout` 兜底（实际实现：新增 `OVERALL_DISPATCH_TIMEOUT`=600s 常量，`post_with_retry` 用 `tokio::time::timeout` 包裹整个重试循环，超时时返回新增的 `TransportError::OverallTimeout`） |

### D. VLM/OCR 输出内容与结构正确性（和"后处理模块强化"直接相关）

> D.1-D.13 全部完成（分七批，均见 `CLAUDE.md` "Post-P10 continued" 对应实现记录）——本节审阅出的全部 13 项 D 类发现均已落地。

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| D.1 | `geometry.rs::rescale_bbox_to_original`（**两路审阅独立发现同一问题**） ✅ | `original_wh` 为 0（畸形/退化光栅化页面）时除零产生 `inf`/`NaN`，`as i32` 静默饱和成 0，dots-ocr 的所有 block 坐标会静默塌缩成 `[0,0,0,0]`，**没有任何警告** | 前置检查 `original_wh` 非零，异常时返回 `PageError` 而不是继续算下去（实际实现：函数改为返回 `Option<[i32;4]>`，`None` 时 `dots_ocr.rs` 跳过该 cell 并 `ctx.warn()`，不中断整页） |
| D.2 | `output_parse.rs`（mineru-vlm 的 `parse_custom_tokens`） ✅ | 坐标超出 `[0,1000]` 时**整块直接丢弃**，而不是像 MonkeyOCRv2 那条路径（`map_bbox_0to1000_clamped`）那样做 clamp/换轴——VLM 量化误差让坐标越界 1-5 个单位是常见情况，尤其在页面边缘 | 改成 clamp 而不是丢弃，只在真正无法解析时才跳过（实际实现：`parse_custom_tokens` 对超出 `[0,1000]` 的坐标做 `min(1000)` clamp，仅当真的发生了 clamp 才发警告，格式良好的行保持静默） |
| D.3 | `output_parse.rs::LiteralParser::parse_string`（MonkeyOCRv2 用的手写 Python-literal 解析器） ✅ | 转义处理只认 `\n`/`\t`/`\r`，遇到 `\u`/`\x` 等其它转义**直接丢掉反斜杠**，把 `中` 这种序列静默变成 `4e2d`——这是**真实文本内容的静默数据损坏**，不是解析失败 | 补上 `\uXXXX`/`\xHH` 解码；未知转义应该保留反斜杠而不是吞掉（实际实现：新增 `LiteralParser::decode_hex_escape`，解码 `\uXXXX`/`\xHH`，失败时回退保留字面量且不消费后续字符；未知转义统一保留反斜杠） |
| D.4 | dots-ocr 全链路（`output_parse.rs` cells_from_values → `geometry.rs` → `dots_ocr.rs`） ✅ | 是唯一一个**从头到尾没有任何 bbox 校验/clamp** 的协议——MonkeyOCRv2 用了防御性的 `map_bbox_0to1000_clamped`，mineru-vlm 的正则解析在更早阶段就拒绝了越界坐标，dots-ocr 两头都没有 | 抽一个共享的 `geometry::sanitize_bbox_px` 并在四个 VLM/OCR adapter 里统一调用（实际实现：新增 `geometry::sanitize_bbox_px`，复用 `map_bbox_0to1000_clamped` 已验证的换轴+clamp+最小1px 逻辑，接入 `dots_ocr.rs`；mineru-vlm/monkeyocr-v2 已有等效校验暂不替换） |
| D.5 | `imaging.rs::crop` ✅ | 裁剪区域完全越界（上游坐标系 bug 导致）时 `.max(1)` **硬造一个 1×1 的图片**送给模型，而不是报错让调用方感知 | 改成返回 `Option<RgbImage>`，越界时让调用方决定跳过/警告（实际实现：`imaging::crop` 改为 `Option<RgbImage>`，bbox 与图像完全不重叠时返回 `None`；`ParseCtx::crop` 转换成 `Err(String)`，三个已有调用点（mineru-vlm/monkeyocr-v2/pipeline）本就按 block 级隔离处理该 `Err`，无需改动调用点逻辑） |
| D.6 | `otsl.rs`（xcel token 解析） ✅ | 上下左右邻居的表格 span 解析冲突时**静默选一边**，不做任何一致性校验或警告——这正是这个模块自称"非官方验证的尽力而为实现"最容易在真实不规则表头上暴露问题的地方 | 冲突时优先选跨度更大的一边，并发一条警告而不是静默决定（实际实现：`xcel` 分支比较 up/left 两个候选referent cell 的当前 span，选更大的一侧并推入警告；`to_html` 签名改为返回 `(String, Vec<String>)`，四个调用点均已接入 `ctx.warn()`） |
| D.7 | `otsl.rs::to_html` ✅ | 只要字符串以 `<table` 开头就整体当成可信 HTML 直接透传，**不检查是否真的以 `</table>` 结尾** | 加上结尾校验，否则退回 OTSL token 化路径（实际实现：仅当 `starts_with("<table")` 且 `ends_with("</table>")` 才信任透传；否则退回 OTSL 分词并发警告） |
| D.8 | `category_map.rs` ✅ | 只有 `map_mineru_vlm_category` 在匹配前做了大小写规范化（因为上游已经统一转小写），`map_dots_ocr_category`/`map_monkeyocrv2_category` 都是精确 Title-Case 匹配——`list_item` 这次能在 mineru-vlm 上被发现纯粹是因为它落到了"未知类别"分支并打了警告；dots-ocr/MonkeyOCRv2 如果发生类似的词表漂移（比如某次模型更新把 `"Text"` 吐成 `"text"`），**同样会静默落进 unknown，没有任何一致的防线** | 三个 mapper 统一加大小写/连字符规范化，而不是只有一个有（实际实现：新增共享 `normalize_key`（小写化 + 剔除 `-`/`_`/空格），`map_mineru_vlm_category`/`map_dots_ocr_category`/`map_monkeyocrv2_category`/`map_pipeline_category` 四个 mapper 统一在匹配前调用） |
| D.9 | `formula_repair.rs::balance_brackets` ✅ | `{`/`}` 和 `\left`/`\right` 各自独立计数，不追踪配对顺序/类型——像 `\left[ \frac{a}{b} \right)` 这种数量配平但语义错配的情况修复不了 | 改成单一有序栈，同时追踪 delimiter 类型（实际实现：经核实 `\left[a,b\right)` 是合法 LaTeX（半开区间记号），不应"修复"成类型匹配——改为修复一个真实存在的不对称缺口：之前只处理"开多于闭"方向，"闭多于开"（如多出的 `}` 或孤立的 `\right`）完全没有处理，现已对称支持双向补齐） |
| D.10 | `dots_ocr.rs` vs `mineru_vlm.rs`/`monkeyocr_v2.rs` ✅ | dots-ocr 的公式输出**没有包一层 `\[...\]`/`$$...$$`**，另外两个协议都包了——同一个 `Block.latex` 字段，不同协议产出的内容形态不一致，下游渲染消费的时候会不一致 | 在 adapter 边界统一，或者交给（P0 新增的）`content_normalize.rs` 统一处理（实际实现：新增共享 `formula_repair::wrap_display_math`（`\[...\]`，与 `render.rs` 直接透传 `Block.latex` 的行为兼容），dots-ocr 接入；MonkeyOCRv2 保留自己真实上游确认过的 `$$...$$` 包装，未强行统一，但补上了上游本身就有、这次移植时遗漏的幂等检查，避免二次包装） |
| D.11 | `output_parse.rs` 三个解析器（custom_token/strict_json/python_literal） ✅ | 容错层级不对称：`strict_json` 有 5 级修复、`python_literal` 有 2 级，**`custom_token`（mineru-vlm 用，也是本次真实验证中唯一发现过词表漂移的格式）反而是 0 级**，一行解析失败就整行跳过 | 至少给 custom_token 补一级"能抢救的先抢救、坐标 clamp、发警告"，而不是整行作废（实际实现：新增 `LAYOUT_LINE_RELAXED_RE` 作为一级回退——去掉行首尾锚定、`<\|ref_end\|>` 变可选，salvage 截断/带垃圾前缀但核心 box 信息完整的行，并发"rescued"警告） |
| D.12 | `monkeyocr_v2.rs` 布局调用 ✅ | `max_tokens: 4096` 固定值，dots-ocr 同类调用用的是 `32768`——版面密集的页面（比如表格多的财报）容易在 4096 就被截断，且**没有任何截断信号**，容错解析器会默默返回截断后能解析的前缀 | 提高默认值到接近 dots-ocr 的量级，并检测 `finish_reason == "length"` 时发出警告（实际实现：核实真实上游 `core_runner.py::get_layout()` 本身就固定用 `max_tokens=4096`——这是协议本身的真实限制，不是移植错误，调高数值反而会偏离真实协议保真度，因此**未改动数值**；只做了建议里真正有价值、无风险的部分——新增共享 `adapters::is_truncated_response` 检测 `finish_reason == "length"`，接入 monkeyocr-v2 布局调用点并发警告） |
| D.13 | `mineru_vlm.rs`/`dots_ocr.rs`/`monkeyocr_v2.rs` 的 `extract_content()` ✅ | 模型返回错误信封/内容审核拒绝/`content: null` 等任何非纯字符串场景，`.unwrap_or("")` 一律当成"模型合法返回了空字符串"——版面检测阶段会静默产出 0 个 block（整页看起来是空白页，不报错）。对比 `pipeline.rs`/`paddleocr.rs` 用 `serde_json::from_value` 对同类"响应形状不对"场景会正确抛 `PageError` | 三个 chat 类 adapter 应该检查 content 字段确实存在，不存在时带上原始响应的 `error`/`finish_reason` 抛错，而不是静默降级成空（实际实现：新增共享 `adapters::extract_chat_content`，区分「choices[0]缺失」/「backend error」/「非stop的finish_reason」/「content显式为null」四种情况，返回可读诊断信息；stage1/单轮调用失败即整页 `PageError`，stage2 每 block 调用失败仅该 block 标记 `error`） |

### E. 阅读顺序算法（`reading_order.rs`）

- **窄装订线场景会退化成错误的行交错顺序**：`split_by_gap` 的合并判断用的是零容差的 `start < band_end`，真实双栏 PDF 装订线很窄、或 OCR bbox 本身带 padding 导致相邻两栏 x 区间刚好碰到/重叠 1px，就会被误判成"无法列切分"，退化到 `(y0,x0)` 全局排序——这恰好是"先读左栏再读右栏"和"逐行左右交错读"两种顺序里**更差的那种**，而且是这个 fallback 本来就是为了避免的失败模式。建议合并判断加一个小容差阈值，"纠缠"兜底路径也应该先按列聚类再排序，而不是直接摊平成 `(y,x)`。
- **完全没有旋转感知**：mineru-vlm 的语法本身带旋转角度信号（`output_parse::parse_rotation`），但 `reading_order.rs`（`paddleocr`/`pipeline` 在用）完全不消费这个信号——扫描件上一个旋转 90° 的印章/侧栏会按未旋转的 bbox 参与排序，大概率插进正文段落中间。

### F. CLI 参数与行为一致性

| # | 位置 | 问题 |
|---|---|---|
| F.1 | `cli.rs` | `--layout-backend`/`--ocr-backend`/`--formula-backend` 传了非 `local` 的值（比如显式传 `remote`）会被**校验通过后直接丢弃**——`PipelineConfig` 里这三个字段的值从没被 `PipelineAdapter::apply_config` 读取过，只有 `table_backend` 真正生效。三个 flag 目前的实际语义是"要么报错、要么被无视"。 |
| F.2 | `cli.rs`/`api.rs` | `--window-size 0`/`--max-concurrency 0` 被 `.max(1)` 静默夹到 1，而不是报 usage error——和已有的 `--layout-backend local` 校验风格不一致。 |
| F.3 | `cli.rs::run_doctor` | `doctor pipeline --endpoint xxx` 里的 `--endpoint` 被完全忽略（`pipeline` 分支在检查 `endpoint` 之前就已经 return 了本机资源建议），且不报错提示这个组合无意义。 |
| F.4 | `api.rs` | 没有和 `cli.rs --stream` 对等的流式接口——Node/Python 调用方要做增量进度展示，只能自己重新实现一遍 CLI 的调度逻辑。 |

### G. 安全边界

- `cli.rs::run_doctor` 对 `--endpoint`（完全用户可控）做真实的 `GET` 请求，没有任何 scheme/host 限制——如果 `uparser` 未来被包装成一个对外服务（比如"Agent 工具"场景，被信任度较低的调用方触发 `doctor`/`parse --endpoint`），这是一个直白的 SSRF 探测点（探内网 `169.254.169.254`、内部端口等）。目前定位是本地 CLI 工具，风险可接受，但如果有服务化计划，需要在那之前补上白名单/内网地址拦截。

### H. IR/Schema 相关

- `types.rs::DocumentProfile` 的 `TableSubtype`/`ChartSubtype` 字段设计上就是"L3 专属、目前永远是 `None`"，但这个事实只写在 Rust doc comment 里——一个只看 JSON 的 Agent 消费方拿到 `"table_subtype": null` 完全看不出这是"设计上永远为空"还是"这次没识别出来"，建议 JSON 里补一个显式的 `l3_available: false` 之类的信号。
- `router.rs`/`profiler.rs` 里一串阈值（`table_frac > 0.5`、`text_density >= 0.15`、`avg_image_density > 0.3` 等）都是硬编码魔数，代码自己的注释也承认"最终路由选择还需要真实样本评估"——比如一份 40% 页面是表格、其余是正文的文档，因为没过 50% 这条线，永远不会被路由到有专门表格识别能力的 `pipeline`，只能退回通用 `mineru-vlm`。
- `render::to_content_list`/`to_json` 没有纯文本抽取模式（只要 `text`，不要 HTML/LaTeX），`--format markdown` 也会把表格的原始 HTML 不加处理地混进"markdown"文档里，导致输出并不是真正意义上的纯 Markdown。

---

## P1：`robustness.rs` 接入真实 adapter 调用链 ✅ 已完成（见 `CLAUDE.md` "Post-P10 continued" 第十批实现记录）

> 实现与设计有一处关键差异，是本节自己"注意边界"提醒之后又踩了一次的坑：下面示例代码里 `Err(_) => String::new()` 会把网络错误伪装成"合法的空字符串"，而 `is_degenerate` 对短字符串（<12 字符）直接返回 `false`——这会导致真实的网络/解析失败被静默当成"这次请求成功且没有退化"，是这次 D.13 刚修过的同一类 bug。实际实现改为：只有在**已经拿到一次真实成功响应**之后才进入 robustness 重试路径（第一次 dispatch 的错误处理完全不变，仍然正常传播为 `PageError`/block `error`）；重试路径本身的失败会回退到"上一次已知内容"而不是空字符串，保证 `is_degenerate` 检查始终看到有意义的输入。范围也从"先只接 mineru_vlm.rs"扩大到 mineru-vlm + monkeyocr-v2（两者结构相同，改动量相近）；dots-ocr 因为是单轮整页调用（不是逐 block），不适合套用同一个"per-block 重试"模式，留待以后单独设计。

### 问题

`is_degenerate()` + `retry_with_temperature()` 从 P1 建成后就是"设计了但没人用"的状态，`CLAUDE.md` 里至少 3 处提到"仍未接入任何 adapter"。这恰恰是真实模型调用最该有的保护——本次两份真实文档运气好没有触发退化输出，但这只是运气。

### 设计

在 `mineru_vlm.rs`（以及未来 `pipeline.rs`/`monkeyocr_v2.rs`）的 stage-2 内容识别循环里，把裸的 `ctx.dispatch(req).await` 换成：

```rust
let content = robustness::retry_with_temperature(
    &robustness::RetryPolicy::default(),
    0.0,
    |temperature| async move {
        let mut req = self.request(...);
        req.sampling["temperature"] = json!(temperature);
        match ctx.dispatch(req).await {
            Ok(resp) => extract_content(&resp).unwrap_or_default().to_string(),
            Err(_) => String::new(), // 网络错误交给外层 transport 的重试处理，这里只处理"成功返回但退化"的情况
        }
    },
).await;
```

**注意边界**：`robustness.rs` 处理的是"HTTP 请求成功但模型输出退化"（重复 token 循环），和 `transport.rs` 已有的"HTTP 请求失败重试"（5xx/超时）是两个正交的问题，不要混在一起——`transport.rs` 的重试保持不变，`robustness.rs` 只包一层在"拿到内容之后、判断是否要整个重新请求"这一步。

### 影响范围

- `mineru_vlm.rs`：stage-2 循环改动，需要新增测试证明"退化输出触发重试、非退化输出不重试"（用 `MockDispatch` 按 key 顺序 seed 多个响应）。
- 是否要接入 `pipeline.rs`/`monkeyocr_v2.rs` 待定——建议先只接 `mineru_vlm.rs` 一个，跑一段时间观察真实收益（比如统计 `capability_notes`/`warnings` 里记录了多少次触发重试），再决定要不要铺开到其他两个二阶段协议。

### 测试要点

- 离线：`MockDispatch` 对同一个 key seed 两次响应（第一次退化、第二次正常），断言最终 block 的 `text` 是第二次的内容，且请求确实发了两次（可以数 seed 消耗次数或者加一个调用计数器）。
- 边界：seed 全部退化（耗尽 `max_attempts`），断言最终仍然返回最后一次内容而不是 panic 或空字符串（保持 `retry_with_temperature` 现有"总会返回"的契约）。

---

## P1：CLI 参数瘦身——配置文件 + 协议预设

### 问题

真实调用长这样（本次会话原样出现过）：

```bash
./target/debug/uparser parse doc.pdf --protocol mineru-vlm \
  --endpoint http://127.0.0.1:19122/files/parser/v1/chat/completions \
  --model MinerU2.5-2604-1.2B --max-concurrency 16 --format markdown > out.md
```

每次都要重复 `--endpoint`/`--model`，容易打错（本次会话出现过一次输出路径打错的情况，虽然不是这个问题但同类风险存在）。

### 设计

新增可选配置文件 `~/.config/uparser/config.toml`（或环境变量 `UPARSER_CONFIG` 指定路径），格式：

```toml
[protocols.mineru-vlm]
endpoint = "http://127.0.0.1:19122/files/parser/v1/chat/completions"
model = "MinerU2.5-2604-1.2B"

[defaults]
max_concurrency = 16
format = "markdown"
```

加载优先级：**CLI flag 显式传入 > 配置文件 > 内置默认值**（和大多数 CLI 工具的约定一致）。配置文件本身完全可选——不存在时行为和现在完全一样，不引入任何 breaking change。

实现上加一个 `config.rs`：

```rust
#[derive(Debug, Default, Deserialize)]
pub struct UparserConfig {
    #[serde(default)]
    pub protocols: HashMap<String, ProtocolDefaults>,
    #[serde(default)]
    pub defaults: GlobalDefaults,
}

pub fn load_config() -> UparserConfig { /* 读文件失败/不存在都返回 Default，不报错 */ }
```

`cli.rs::run_parse` 在构造 `AdapterOverrides`/`ParseOptions` 之前，先用 `load_config()` 的值填充"用户没有显式传 flag"的字段。判断"用户是否显式传了 flag"用 `Option<T>`（`clap` 已经是这个模式了），配置文件的值只在 `None` 时生效。

### 测试要点

- `config.rs` 纯函数单测：正常 TOML 解析、文件不存在时返回 Default、TOML 语法错误时返回 Default 并打印一条 warning 到 stderr（不能因为配置文件损坏就让 `parse` 直接失败）。
- CLI 集成测试：设 `UPARSER_CONFIG` 指向一个临时 TOML 文件，跑 `parse --protocol mineru-vlm`（不传 `--endpoint`），断言实际使用了配置文件里的 endpoint（复用现有"连接被拒绝端口，从错误信息里看到端口号"的验证手法）。

---

## P2：Markdown 渲染增强

### 问题

当前 `render::to_markdown`：

```rust
if let Some(html) = &block.html { ... }
else if let Some(latex) = &block.latex { ... }
else if let Some(text) = &block.text { out.push_str(text); }
```

不区分 `category`，标题不加 `#`，列表不加 `-`，`page_number`/`footer`/`footnote` 类别的 block 原样混进正文（本次真实输出里 `-1-` 到 `-7-` 这种页码就是这样出现的）。

### 设计

按 `category` 分派渲染策略，新增一个 `render::markdown::render_block(block: &Block) -> Option<String>`（返回 `None` 表示这个 block 不应该出现在正文里）：

| category | 渲染 |
|---|---|
| `title` | 按启发式分级（比如以往连续出现的 title 长度/位置作为分级信号；没有更好信号时先统一用 `#`，分级是可选增强） |
| `list` | 保留原文本（block 本身已经是合并后的列表容器，暂不逐条拆分） |
| `page_number` | **默认剔除**，不进入正文；`--keep-page-numbers` flag 可选保留（有些场景需要保留原始页码做引用） |
| `footer`/`footnote` | 移到文档末尾统一渲染成脚注区块，而不是原地混排（可选增强，工作量较大，可以先只做"剔除 page_number"这一步） |
| 其余（`text`/`table`/`equation`/...) | 保持现有逻辑 |

**建议分两步走**：第一步只做"`page_number` 默认从 Markdown 正文剔除"（低风险、高信噪比提升，本次真实输出直接证明了价值）；标题分级和脚注重排作为后续增强，不在这次范围内强求。

### 测试要点

- 现有 `render.rs` 的 `insta` 快照测试需要更新（`page_number` 剔除后快照会变，这是预期的，用 `cargo insta review` 接受新快照）。
- 新增测试：一个包含 `text`/`page_number`/`title` 混合的 `ParseResult`，断言 `to_markdown` 输出里不包含 `page_number` 的文本、且 `--keep-page-numbers` 时包含。

---

## P3（低优先级，仅记录不建议本轮做）

- **`cli.rs`/`api.rs` 逻辑统一**：P10 就已经文档化这个重复，风险是"动 `cli.rs` 已经过测试的控制流"，建议等 P0/P1 的改动稳定之后再做一次专门的重构 pass，而不是夹在这次一起改。
- **`packages/node`/`packages/python` 打包脚手架**（T-10.3）：`opensource/liteparse` 的 `packages/` 已有现成模板可以直接抄，工作量主要是体力活，不涉及设计决策，可以随时单独排期。
- **`uparser doctor` 增加 dry-run 探测**（比如探测 `pipeline` 四个 stage 各自的端点，而不只是 `pipeline` 整体的本机资源建议）：等 Pipeline Model Serving 真的有参考部署（T-5.7 仍未完成）之后再做更有意义。

---

## 实施顺序建议

1. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 记录）—— P0-最优先：三个"从未接入"能力一次性接线**——`postprocess.rs`（2.1 节）+ `ingest::ingest_document`（深度审阅 A.1）+ `ParseResult.warnings` 真正被写入（深度审阅 A.2，`capability_notes` 仍未接入，留待后续）。三者共性都是"正确代码已经写好测好，缺的是调用点"，改动量小、风险低、收益最大，其中 `ingest_document` 未接入是本轮审阅发现的最严重问题（非 PDF 输入静默产出完全错误的结果且 exit 0），已第一批修完并通过真实端点回归验证（212 unit + 23 CLI + 2 contract 测试全绿）。
2. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第二批实现记录）—— P0：并发/资源风险收敛**——B.1（panic 隔离到单页，`page_panic_error`+`JoinError` 匹配）、B.4/B.5（LibreOffice 子进程 `kill_on_drop(true)` + 独立 `UserInstallation` profile 目录）、B.2（permit 从"crop/resize/encode 之前"收紧到"`dispatch`/`dispatch_rest` 调用前一刻"，`mineru_vlm.rs`/`monkeyocr_v2.rs`/`pipeline.rs` 三处）均已完成并通过真实端点回归验证。B.3（抽取共享 `fan_out_stage2` 辅助函数）**明确推迟**——评估为"改动价值不足以覆盖三个 adapter 结构差异带来的重构风险"，记录在案，未来如果再新增第四个多阶段 adapter 可重新评估。
3. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第三批实现记录）—— P0：进度可观测性 + `--pages`**：直接源于本次真实使用的具体痛点，改动集中在 `scheduler.rs`/`cli.rs`，风险可控，且回调式设计保证向后兼容（`run()` 委托给 `run_with_progress` 并传入空回调）。
4. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第四批实现记录）—— P0：VLM 输出正确性收敛（深度审阅 D 类，优先项）**——D.1（`rescale_bbox_to_original` 除零静默饱和，两路审阅独立发现）、D.3（MonkeyOCRv2 `\u`/`\x` 转义静默丢字/损坏文本）、D.4（dots-ocr 完全没有 bbox 校验，新增共享 `geometry::sanitize_bbox_px`）、D.13（chat 类 adapter 把畸形响应当空字符串，新增共享 `adapters::extract_chat_content`）均已完成并通过真实端点回归验证（249 unit + 24 CLI + 2 contract 测试全绿，`--features native`）。
4b. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第五批实现记录）—— P0：VLM 输出正确性收敛第二批**——D.2（mineru-vlm 坐标越界改 clamp 而不是丢弃整块）、D.5（`imaging::crop` 越界不再硬造 1×1 占位图，改 `Option<RgbImage>`）、D.8（四个 `category_map.rs` mapper 统一加大小写/连字符规范化）均已完成并通过真实端点回归验证（255 unit + 24 CLI + 2 contract 测试全绿，`--features native`）。
4c. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第六批实现记录）—— P0：表格解析正确性收敛**——D.6（`otsl.rs` xcel span 冲突改为按更大 span 决策并发警告，而不是静默选边）、D.7（`otsl::to_html` 只有 `</table>` 结尾校验通过才信任 HTML 透传，否则退回 OTSL 分词）均已完成并通过真实端点回归验证（258 unit + 24 CLI + 2 contract 测试全绿，`--features native`）。
4d. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第七批实现记录）—— P0：公式/LaTeX 处理收敛**——D.9（`balance_brackets` 补齐"闭多于开"方向的对称处理，同时确认 `\left[...\right)` 类型不匹配是合法 LaTeX 不应"修复"）、D.10（新增共享 `formula_repair::wrap_display_math`，dots-ocr 接入统一 `\[...\]` 包装；MonkeyOCRv2 保留真实上游确认过的 `$$...$$`，但补上遗漏的幂等检查）均已完成并通过真实端点回归验证（259 unit + 25 CLI + 2 contract 测试全绿）。
4e. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第八批实现记录）—— P0：D 类收尾**——D.11（`custom_token` 解析器补齐一级 relaxed-match 回退，与 strict_json/python_literal 的容错层级对齐）、D.12（核实真实上游 `max_tokens=4096` 本身就是协议真实限制而非移植缺陷，未强改数值；新增共享 `is_truncated_response` 检测 `finish_reason: length` 并接入 monkeyocr-v2 布局调用点）均已完成并通过真实端点回归验证（266 unit + 25 CLI + 2 contract 测试全绿，`--features native` 273 unit）。**深度审阅 D 类全部 13 项发现均已实施完毕**，剩余待办转向 C/F/G/H 类及 `content_normalize.rs`/`robustness.rs` 接入等结构性项目。
5. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第十一批实现记录）—— P0：`content_normalize.rs`（标点规范化）**——新模块 `normalize_punctuation`（CJK 占比阈值门控 + 小数点保护）+ `collapse_whitespace`，接入 `postprocess::merge_paragraphs_by_geometry`，对每个 `text` 非空 block 在合并前生效。已用真实案例在真实端点验证（291 unit + 25 CLI + 2 contract 测试全绿，`--features native` 298 unit）。D.10（LaTeX 包裹不一致）已在更早批次完成（见 D 类记录）。跨页合并（2.3）风险较高（要碰 IR），仍单独留待后续迭代。
6. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第九批实现记录）—— P1：网络重试鲁棒性（C 类）**——C.1（429 与 5xx 同等重试，解析 `Retry-After`）、C.2（`jittered_backoff` full-jitter 策略，5s 封顶）、C.3（200+非法 JSON body 计入重试预算）、C.4（`dispatch_rest` 真正读取调用方 `timeout`/`max_retries`，`pipeline.rs`/`paddleocr.rs` 5 处调用点全部接入）、C.5（`OVERALL_DISPATCH_TIMEOUT`=600s 外层兜底）均已完成并通过真实端点回归验证（275 unit + 25 CLI + 2 contract 测试全绿，`--features native` 282 unit）。
6b. ✅ **已完成（见 `CLAUDE.md` "Post-P10 continued" 第十批实现记录）—— P1：`robustness.rs` 接入**——mineru-vlm 与 monkeyocr-v2 的 stage-2 单 block 内容识别循环均已接入 `is_degenerate`/`retry_with_temperature`，只对纯文本类别生效（table/equation 需要结构保真，不套用"看起来在循环"的启发式），且只在已经拿到一次真实成功响应之后才触发，避免把网络错误伪装成"空内容"。dots-ocr（单轮整页调用，不是逐 block）不适用同一模式，留待以后单独设计。已完成并通过真实端点回归验证（278 unit + 25 CLI + 2 contract 测试全绿，`--features native` 285 unit）。配置文件和 CLI 瘦身可以等参数列表继续增长到确实难用时再做。
7. **P2 Markdown 增强 + F 类 CLI 一致性小修**：先做"剔除 page_number"这一个低风险子项，顺带把 F.1（pipeline 后端 flag 静默丢弃）、F.2（`--max-concurrency 0` 该报错却被静默夹到 1）这类"参数校验不一致"的小问题一起清掉，改动都很小。
8. **P3 留档不动**：`cli.rs`/`api.rs` 统一、npm/pip 打包脚手架、E 类阅读顺序算法增强（窄装订线/旋转感知）、G 类 SSRF 加固（等真的有服务化计划再做）、H 类 IR/Schema 细节，记录在案，不在这轮排期。

每一项落地前都应该：先写离线单测 + 一次真实端点回归（复用本次已经验证过的 `MinerU2.5-2604-1.2B` 端点和两份真实 PDF），再更新 `CLAUDE.md` 记录真实验证结果——保持这个项目一贯的"离线测试 + 真实端点验证双轨"的实践。
