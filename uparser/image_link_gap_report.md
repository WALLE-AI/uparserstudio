# 为什么 MinerU 的 Markdown 保留图片链接，而 uparser 没有

> 基于对 `opensource/MinerU`（Python）与 `uparser/crates/uparser-core`（Rust）两侧源码的实读分析，所有结论均标注文件:行号依据，不做推测。

## 1. 结论先行

两边的差距**不是 bug，也不是共享组件的回归**，而是两个相互独立、彼此叠加的设计缺口共同造成的：

1. **IR（中间表示）层面缺一个字段**：`uparser` 的 `Block` 结构体从设计之初就没有任何字段可以承载"已保存的图片路径/URL/裁剪像素"这类信息，`render::to_markdown` 天生就没有第四条渲染分支可以走。
2. **Adapter 移植时把 MinerU 的两步操作合并成了一步**：MinerU 对图片类 block 实际做了两件独立的事——(a) 跳过用模型生成图片描述，(b) **无条件**裁剪并保存原始像素、供 Markdown 引用。uparser 的移植只正确复刻了 (a)，(b) 从未被实现——`skip` 判断发生在裁剪调用之前，图片区域连裁都没裁，更谈不上保存。

这是一个此前从未被记录过的真实缺口——`ARCHITECTURE.md`、`CLAUDE.md`、`CLI_ENHANCEMENT_PROPOSAL.md`（四路并行深度源码审阅，发现 35+ 项问题）里都没有提到过它，不是"已知但暂缓"的权衡。

---

## 2. MinerU 侧：图片是如何被裁剪保存并写入 Markdown 的

### 2.1 裁剪并落盘（VLM 后端与 pipeline 后端共用同一套机制）

- `mineru/utils/pdf_image_tools.py:487-514` —— `cut_image(bbox, page_num, page_pil_img, return_path, image_writer, scale=2)`：把整页 PIL 图像按 `bbox` 裁剪，编码成 JPEG，用 `str_sha256` 算出哈希文件名 `img_hash256_path`，再调用 `image_writer.write(img_hash256_path, img_bytes)`（第 513 行）——这是一次**真实的磁盘写入**（通过 `FileBasedDataWriter`）。函数返回这个哈希后的文件名。
- `mineru/utils/cut_image.py:1-16` —— `cut_image_and_table(span, page_pil_img, page_img_md5, page_id, image_writer, scale=2)`：包装 `cut_image`，把结果写回 `span["image_path"]`（bbox 非法或没有 writer 时写空字符串）。
- `mineru/backend/vlm/model_output_to_middle_json.py:46-50`（VLM 后端）：**无条件**遍历所有 span，只要类型是 `IMAGE`/`TABLE`/`CHART`/`INTERLINE_EQUATION` 就调用 `cut_image_and_table(...)`——**这一步和"模型有没有对这个区块生成文字描述"完全无关，是独立执行的**。pipeline 后端在 `mineru/backend/pipeline/model_json_to_middle_json.py:57` 有完全对应的逻辑。

也就是说，MinerU 把"调用模型抽取内容"和"裁剪原始像素并落盘"当成**两个正交的步骤**——第二步即便第一步被跳过（比如图片类 block 根本不需要模型生成文字）也照样执行。

### 2.2 Markdown 渲染器从保存路径拼出 `![]()` 链接

- `mineru/backend/vlm/vlm_middle_json_mkcontent.py:146-163`（`_render_visual_block_segments`，处理 `BlockType.IMAGE_BODY`）：只要该 span 的 `image_path` 非空，就 `append(f"![]({media_path})")`，其中 `media_path = _build_media_path(img_buket_path, image_path)`（第 88-93 行，即 `f"{img_buket_path}/{image_path}"`）。
- `mineru/backend/pipeline/pipeline_middle_json_mkcontent.py:146-156` 有完全对应的逻辑。
- 两个后端都只在 `make_mode == MM_MD`（多模态 markdown 模式，默认/常用模式）时才走这条渲染路径（`vlm_middle_json_mkcontent.py:380-388`、`pipeline_middle_json_mkcontent.py:44-48`）；在 `NLP_MD` 模式下图片 block 会被显式 `continue` 跳过——也就是说 MinerU **自己也有"图片不出现在 Markdown 里"的模式**，只是它的默认/常用模式不是这个。

### 2.3 这是一个真实的、有磁盘副作用的机制，由 CLI 统一建立目录

- `mineru/cli/common.py:186-191` —— `prepare_env(output_dir, pdf_file_name, parse_method)`：`os.makedirs` 创建 `local_image_dir = <output_dir>/<pdf_file_name>/<parse_method>/images`（第 187-189 行），是每次解析都会无条件创建的真实子目录。
- 多个调用点（`common.py` 第 378-380、451-461、492-502、538-557、593-612、637-660 行，覆盖 pipeline/vlm/office/hybrid 各模式）构造 `image_writer, md_writer = FileBasedDataWriter(local_image_dir), FileBasedDataWriter(local_md_dir)`，并把 `image_writer` 一路传进解析调用链。
- 第 312 行：`image_dir = str(os.path.basename(local_image_dir))` → `"images"`，作为 `img_buket_path` 传给 markdown 生成器（第 315 行），最终拼出输出里看到的相对路径 `images/<hash>.jpg`。

**结论**：MinerU 的 Markdown 输出，图片链接是相对于同级 `images/` 文件夹的——离开这个文件夹单独看 `.md` 文件，图片链接本身就是"断的"，只是内容完整（有文字描述 + 有图片引用两部分）。这不是纯内存、自包含的输出，而是"`.md` + `images/` 文件夹"这个组合产物。

---

## 3. uparser 侧：为什么图片 block 在 Markdown 里"消失"

### 3.1 `render::to_markdown` 只认 `text`/`html`/`latex` 三个字段

```rust
// crates/uparser-core/src/render/mod.rs:12-30
pub fn to_markdown(result: &ParseResult) -> String {
    let mut out = String::new();
    for page in &result.pages {
        for block in &page.blocks {
            if let Some(html) = &block.html {
                out.push_str(html);
                out.push_str("\n\n");
            } else if let Some(latex) = &block.latex {
                ...
            } else if let Some(text) = &block.text {
                ...
            }
            // 没有第四条分支：三者都是 None 时，这个 block 什么都不会输出
        }
    }
    out.trim_end().to_string()
}
```

渲染逻辑完全按 `html → latex → text` 的顺序 fallback，**没有任何按 `category`/`category_raw` 分支的逻辑**，也没有专门"跳过图片"的代码——图片 block 之所以什么都不输出，纯粹是因为它的 `html`/`latex`/`text` 三个字段全是 `None`，落进了渲染函数天生没有覆盖的那个"空隙"里。

### 3.2 `Block` IR 里根本没有能装图片路径的字段

`types.rs` 里 `Block` 的完整字段：`geom`、`geom_frame`、`bbox_px`、`category_raw`、`category`、`reading_order`、`text: Option<String>`、`html: Option<String>`、`latex: Option<String>`、`spans`、`merge_hint`、`confidence`、`source`、`error`。**没有任何字段可以存放"已保存图片的路径/URL"或"裁剪出来的像素"**——这是彻底缺失，不是"有字段但没人填"。

### 3.3 每个 adapter 对图片类别都是"连裁都不裁"

以 `mineru_vlm.rs` 为例（`pipeline.rs` 是完全对应的模式）：

- `adapters/mineru_vlm.rs:38-40`：
  ```rust
  const SKIP_CONTENT: &[&str] = &["image", "list", "equation_block"];
  ```
- `adapters/mineru_vlm.rs:264-269`：stage-2 内容识别的 fan-out 里，`let skip = SKIP_CONTENT.contains(&p.category_raw.as_str());`，如果 `skip` 为真，闭包**立刻**返回 `(index, Ok(None))`——而真正做裁剪的 `ctx.crop(page, p.bbox_px)` 调用在下一行（第 279 行），只在"不跳过"的分支里才会执行。也就是说对图片类 block，像素区域从头到尾都没有被单独裁剪出来过，更谈不上保存。
- `adapters/mineru_vlm.rs:361-369`：`Ok(None)` 的结果最终对应 `(text, html, latex, error) = (None, None, None, None)`——block 本身依然会被正常构造推入结果（`bbox_px`/`category` 都是对的），只是四个内容字段全空。
- `adapters/pipeline.rs:50-53, 236`：`const SKIP_RECOGNITION: &[&str] = &["image", "chart", "discarded"];`，注释里明确写着"和 mineru-vlm 的 `SKIP_CONTENT` 类似"——同样的跳过时机、同样的后果。
- `dots_ocr.rs`/`monkeyocr_v2.rs` 的测试也确认了 `category == "image"` 的 block 是真实存在的，只是（因为 `Block` 里没有对应字段）没有任何像素引用能被保留下来。

这其实是"移植时把 MinerU 的两步操作误合并成了一步"：MinerU 自己也不会为图片调用 VLM 生成描述（这一点 uparser 正确复刻了），但 MinerU 会在此之外**无条件**再裁剪一次像素并存盘（`model_output_to_middle_json.py:46-50`）。uparser 的移植只做了前半句"不调用模型"，后半句"裁剪存盘"完全没有对应实现——大概率是因为（回到第 3.2 节）即便当时想裁剪保存，`Block` 上也没有字段可以承载结果，所以这一步在设计阶段就被隐式地省略掉了。

### 3.4 项目里完全没有"输出资产目录"这个概念

在 `ingest.rs`/`cli.rs`/`api.rs` 及所有 adapter 源码里搜索 `asset`/`images_dir`/`image_dir`/`img_bucket`/`output_dir` 等关键词，除测试/URL 噪音外**没有任何匹配**。整个 crate 里：

- 没有 `--output-dir`/images 文件夹相关的 CLI flag；
- 没有类似 `FileBasedDataWriter` 的落盘写入抽象；
- 没有任何"解析产生一个文件夹的副作用"机制。

`imaging.rs:32` 的 `pub fn crop(...) -> Option<RgbImage>` 是一个通用裁剪原语，确实存在、也确实被用来为 VLM stage-2 请求准备"模型输入用的裁剪图"——但只在"不跳过"分支才会被调用，且即便调用了，裁剪结果（`RgbImage`）也只是被 base64 编码进请求体、发完请求就丢弃——从未落盘，也从未挂到 `Block` 上。

### 3.5 现有设计文档里找不到这个问题的任何记录

- 顶层 `ARCHITECTURE.md` 全文搜索 "asset"/"output dir"/"output directory" **零匹配**——顶层设计文档从一开始就没有规划过"图片输出文件夹"这类机制。
- `uparser/CLI_ENHANCEMENT_PROPOSAL.md`（这次会话里做的四路并行深度源码审阅，发现 35+ 项问题）确实有一节"P2：Markdown 渲染增强"（第 383-416 行），提出要给 `to_markdown` 加按 category 分派的逻辑——但它列出的类别表（第 401-407 行）只覆盖 `title`/`list`/`page_number`/`footer`/`footnote`/"其余(text/table/equation/...)"，**表格里完全没有出现 `image`/`picture`**。那一节举的"渲染器现状不足"的真实例子是页码字符串（`"-1-"`...`"-7-"`）混进正文，而不是图片缺失。
- 对整份提案文档搜索 "image"/"asset"/"picture"，只命中了不相关的条目（B.4 LibreOffice/ImageMagick 子进程卫生、D.5 `imaging.rs::crop` 越界处理）——没有一处提到"图片类 block 渲染出空内容"这件事。

也就是说，即便是这次会话里对全部 ~25 个源文件做的四路并行深度审阅，也没有发现这个问题——它是一个真正意义上"此前完全没被记录过"的缺口。

---

## 4. 修复思路（暂未实施，供后续排期参考）

如果要对齐 MinerU 的行为，至少需要以下几处改动（涉及 IR，风险高于本项目近期做过的大多数小修，建议单独排期，而不是顺手改）：

1. **`types.rs::Block` 扩展一个可选字段**，例如 `asset_path: Option<String>` 或更通用的 `asset: Option<BlockAsset>`（携带原始字节或已写入的相对路径二选一，需要先确认 IR 设计方向）。
2. **在各 adapter 的 `SKIP_CONTENT`/`SKIP_RECOGNITION` 分支之前，为 `image`/`picture`/`chart` 类别单独调用一次裁剪**（复用已有的 `imaging::crop`），把结果写入新增字段——这一步要跳过模型调用但不能跳过裁剪，和现在"一个 `skip` 判断管两件事"的写法不同。
3. **新增一个类似 MinerU `image_writer` 的落盘抽象**（或者更简单地：先只在 `--format markdown` 且指定了输出目录时才落盘，避免给单纯要 JSON/stdout 输出的调用方引入不必要的磁盘副作用——这一点上 uparser 的"Agent-first CLI 契约"（stdout=结果）和 MinerU"总是往磁盘写文件夹"的设计哲学本身就有冲突，需要先决定 uparser 要不要引入这种副作用，还是只在 IR 里保留 base64/字节、完全不落盘，把"要不要写文件"交给调用方决定）。
4. **`render::to_markdown` 增加第四条分支**，当 `text`/`html`/`latex` 都为空但 `asset` 字段有值时，输出 `![](...)` 引用。

其中第 3 点是最需要先做设计决策的地方：uparser 的 CLI 契约从 P9 起就明确"stdout=结果、stderr=日志"（`ARCHITECTURE.md` §6.1），如果要像 MinerU 一样默认写一个 `images/` 文件夹副作用，需要先确认这和现有契约是否冲突，或者改为把图片数据内嵌在 JSON 里（比如 base64），由调用方自己决定要不要落盘——这是一个真正的设计选择，不是纯粹的实现细节。
