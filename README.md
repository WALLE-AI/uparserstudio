# uparserstudio

**统一文档解析 CLI(Rust)** —— 把 PDF / Word / PPT / Excel / 图片解析成干净的 **Markdown** 或结构化 **JSON**(带 bbox、类别、表格、公式、阅读顺序的 block)。专为**编码 Agent 作为子进程驱动**而设计:`stdout=结果`、`stderr=日志`、`exit code=语义化`。

支持六种可插拔解析协议:从**零模型纯 Rust**(`native`,~ms/页、无 GPU)到**视觉大模型**(`mineru-vlm`,质量最佳)。

---

## ✨ 亮点

- **一个 CLI,六种协议**:`native` / `mineru-vlm` / `dots-ocr` / `monkeyocr-v2` / `pipeline` / `paddleocr`,外加 `auto`(自动路由)。
- **Agent-first 契约**:stdout 只放结果,stderr 放日志,exit code 0–4 语义化,JSON 错误结构化。
- **模型推理一律外置**(vLLM/LMDeploy OpenAI 兼容端点或轻量 REST),不在进程内跑重模型。
- **多语言绑定**:同一 Rust core,提供 Node.js(napi-rs)与 Python(PyO3)绑定。
- **实测领先**(opendataloader-bench,200 篇真实 PDF,详见 [`BENCHMARK_REPORT.md`](BENCHMARK_REPORT.md)):

  | 引擎 | Overall | 阅读顺序 | 表格 | 标题 | 速度 |
  |---|---|---|---|---|---|
  | **uparser · mineru-vlm** | **0.9284** 🥇 | 0.947 | 0.944 | 0.878 | 1.81 s/篇(GPU) |
  | **uparser · native**(零模型) | 0.8754 | 0.915 | 0.814 | 0.788 | **0.046 s/篇** |
  | liteparse(对比) | 0.576 | 0.866 | 0.000 | 0.000 | 1.061 s/篇 |

  mineru-vlm 集成**全场第一**(超榜首 hybrid 0.907);native 纯 Rust 零模型在精度与速度上**全面超越 liteparse**。

---

## 🚀 快速开始

```bash
cd uparser

# 构建(native:纯 Rust,无需下载 PDFium)
cargo build --release --features native
# 或加 pdfium(VLM/OCR 协议光栅化页面所需;首次联网下载 PDFium ~2min)
cargo build --release --features native,pdfium
# 产物:uparser/target/release/uparser
```

```bash
# ① 电子版 PDF → Markdown(快、本地、无 GPU)
uparser parse --protocol native --format markdown report.pdf > report.md

# ② 视觉模型解析(需 OpenAI 兼容 vLLM 端点)
uparser parse --protocol mineru-vlm \
  --endpoint http://127.0.0.1:19122/v1/chat/completions \
  --model MinerU2.5-2604-1.2B \
  --format markdown scan.pdf > scan.md

# ③ 自动选引擎
uparser parse --protocol auto --format markdown mystery.pdf > out.md

# 其他:先分类(不调模型)/ 探活 / 列能力
uparser classify paper.pdf
uparser doctor mineru-vlm --endpoint <url>
uparser protocols
```

输出契约:`exit 0` 成功、`1` 用法错误、`2` 依赖/环境错误、`3` 部分成功(看 JSON 里 `page_errors`)、`4` 内部错误。完整用法见 [`UPARSER_GUIDE.md`](UPARSER_GUIDE.md)。

---

## 📦 包的情况(仓库结构与 crate)

本仓库是一个**研究工作区**,包含产品代码(`uparser/`)、设计/评测文档、以及一个用于对比研究的第三方项目并置目录(`opensource/`,**已 gitignore,不随仓库上传**)。

```
uparserstudio/
├── README.md                     ← 本文件(GitHub 首页)
├── UPARSER_GUIDE.md              完整使用文档 + 架构图/时序图
├── ARCHITECTURE.md               目标设计(v0.9,权威)
├── DEVELOPMENT_PLAN.md           分期任务(P0–P10)
├── BENCHMARK_REPORT.md           opendataloader-bench 实测报告
├── skills/uparser/               Claude Code Skill(SKILL.md + 包装器 + 配置模板)
├── opensource/                   第三方上游克隆(gitignore,仅本地对比研究用)
└── uparser/                      ★ Rust workspace(真正的产品)
    ├── Cargo.toml                workspace(edition 2024)
    └── crates/
        ├── uparser-core/         核心库 uparser_core + CLI 二进制 uparser
        ├── uparser-napi/         Node.js 绑定(napi-rs)→ cdylib
        ├── uparser-python/       Python 绑定(PyO3)→ cdylib (_uparser)
        └── uparser-native-engine/ 内部化的纯 Rust 解析引擎(MIT,见 ATTRIBUTION.md)
```

### Crate 一览

| Crate | 版本 | 类型 | 说明 | License |
|---|---|---|---|---|
| `uparser-core` | 0.1.0 | lib + bin(`uparser`) | 核心解析引擎、六协议适配器、调度、缓存、CLI | UNLICENSED |
| `uparser-napi` | 0.1.0 | cdylib + rlib | Node.js 绑定,导出 async `parse`/`classify` | UNLICENSED |
| `uparser-python` | 0.1.0 | cdylib(`_uparser`)| Python 绑定,导出 `parse`/`classify` | UNLICENSED |
| `uparser-native-engine` | 0.1.7 | lib | `native` 协议的纯 Rust 引擎(lopdf,无 PDFium/无 OCR),内部化自 firecrawl/pdf-inspector | **MIT** |

### 构建特性(features,均非默认)

| Feature | 作用 | 备注 |
|---|---|---|
| (default) | 仅编译 mock 及需外置端点的协议 | 全绿、无需网络 |
| `native` | 启用零模型 `native` 协议(拉入 `uparser-native-engine`) | 纯 Rust,无 PDFium 下载 |
| `pdfium` | 页面光栅化(VLM/OCR 协议所需),经 `liteparse-pdfium` | 首次构建从 GitHub 下载 PDFium 二进制 |
| `pipeline-local-table` | `pipeline` 协议的本地 ONNX 表格推理(`ort`) | 需 glibc ≥ 2.38 |

### 构建 / 测试 / lint(在 `uparser/` 下)

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

默认全绿,无需额外 flag 或网络访问。

---

## 🤖 作为 Claude Code Skill 使用

`skills/uparser/` 是一个可直接安装的 Claude Code Skill。**只需装 skill,二进制首次使用时自动下载**(无需手动构建):

```bash
# 安装到全局(任意目录可用)
cp -r skills/uparser ~/.claude/skills/uparser
# 或项目级(仅该仓库内可用)
cp -r skills/uparser <项目>/.claude/skills/uparser
```

首次调用时,skill 的 `ensure_uparser.sh` 会按 `PATH → 缓存 → 从 GitHub Release 下载版本固定的预编译包(直连→ghfast.top 镜像兜底,校验 sha256 + 冒烟)→ 源码构建兜底` 解析出 `uparser` 并缓存到 `~/.cache/uparser/bin/`。启动 Claude Code 后 `/uparser` 触发,或直接说「把这个 PDF 转成 Markdown」自动匹配。

**端点配置化**(免每次带 `--endpoint`):把端点写进 `~/.config/uparser/config.toml`(模板见 `skills/uparser/references/config.example.toml`),用包装器调用——`scripts/uparser-run.sh`(Linux/WSL)或 `scripts/uparser-run.ps1`(Windows)——它会先确保二进制就位,再按 `--protocol` 自动注入 `--endpoint`/`--model`:

```bash
skills/uparser/scripts/uparser-run.sh parse --protocol mineru-vlm --format markdown doc.pdf
```

**平台**:预编译包目前为 **Linux x86_64(glibc ≥ 2.35)**。其他平台(arm64 / 旧 glibc / Windows)下载器自动回退到源码构建;Windows 另可用 `scripts/build-windows.ps1`(需 rustup + MSVC)。覆盖变量:`UPARSER_VERSION` / `UPARSER_REPO` / `UPARSER_HOME`。

---

## 📖 文档索引

| 文档 | 内容 |
|---|---|
| [`UPARSER_GUIDE.md`](UPARSER_GUIDE.md) | **完整使用文档**:CLI 全参数、输出契约、JSON 结构、六协议详解、**架构流程图 + 时序图**、评测依据、Skill 使用、FAQ |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 目标设计(Protocol Adapter、共享 Core、Profiler+Router、缓存等,v0.9 权威) |
| [`BENCHMARK_REPORT.md`](BENCHMARK_REPORT.md) | opendataloader-bench 200 篇实测、三路对比、渲染缺陷修复复盘 |
| [`DEVELOPMENT_PLAN.md`](DEVELOPMENT_PLAN.md) | 分期任务 P0–P10 与各 Gate |
| [`skills/uparser/SKILL.md`](skills/uparser/SKILL.md) | Skill 说明与配方 |
| [`skills/uparser/references/protocols.md`](skills/uparser/references/protocols.md) | 协议参考与能力矩阵 |

---

## 📦 分发

- **同平台(Linux x64,glibc ≥ 2.35)**:二进制已静态链接 PDFium、运行时仅依赖 glibc,可直接打包分发(skill + 二进制 + `install.sh`)。建议通过 **GitHub Releases** 附带打包产物,而非提交进 git(`*.tar.gz` 已 gitignore)。
- **不同架构 / glibc < 2.35 / Windows 原生**:在目标机从源码构建。

---

## ⚖️ License 与归属

- 产品 crate(`uparser-core`/`napi`/`python`)当前标记为 **`UNLICENSED`** —— 若要公开开源,请先明确一个 License(如 MIT/Apache-2.0)并更新 `uparser/Cargo.toml` 的 `[workspace.package].license`。
- `uparser-native-engine` 内部化自 **firecrawl/pdf-inspector(MIT)**,保留其 `LICENSE` 与 `ATTRIBUTION.md`(上游 commit 记录在案)。
- `opensource/` 下为各上游项目的独立克隆(各自 License),**不随本仓库分发**(已 gitignore)。
