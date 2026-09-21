# 端点配置化执行方案（ENDPOINT_CONFIG_EXECUTION_PLAN）

> 状态：**已完成**（含收尾补漏，见 §7.4）
> 基线：`feat/architecture-v2` @ `f127729`
> 日期：2026-09-21
> 相关文档：`ARCHITECTURE_V1.0.md` §902/§949、`CLI_ENHANCEMENT_PROPOSAL.md`:346（配置层的最初提案）、`CORE_ARCHITECTURE_REVIEW_AND_REFACTOR_PLAN.md`:243（绑定层缺配置支持的既有记录）

---

## 1. 背景与问题定义

需求：把 `mineru-vlm` / `monkeyocr-v2` / `navidc-ocr` / `dots-ocr` / `generic-vlm` 等协议的服务地址从代码里挪出来，做成配置形式，并随 skill 一起发布。

### 1.1 现状核查结论（先证伪再动工）

调研后必须先纠正一个前提：**端点并非"写死"，基础配置机制已经存在且已随 skill 发布**。

- `uparser/crates/uparser-core/src/agent_config.rs` 已实现 `~/.config/uparser/config.toml`（可用 `$UPARSER_CONFIG` 改路径）的 `[<protocol>]` 段读取。
- 优先级链已实现并在 `protocols.md`:160-169 文档化：`--endpoint/--model` → `UPARSER_ENDPOINT`/`UPARSER_MODEL` → 配置文件 → adapter 内置默认值。
- 查表用的是**路由后的 effective protocol**（`cli.rs`:689-690），所以 `--mode auto` 选中 `mineru-vlm` 会正确读到 `[mineru-vlm]`。
- `skills/uparser/references/config.example.toml` 已随 skill 发布。

因此各 adapter `Default` impl 里的 `http://localhost:8000/...` 是**兜底默认值**，不是不可覆盖的硬编码。本方案不是"从零做配置"，而是**补齐这一层的四个真实缺口**。

### 1.2 四个真实缺口

| # | 缺口 | 证据 |
|---|---|---|
| G1 | `pipeline` 的 9 个分阶段端点**完全无法写进配置文件**，只能逐次用 `--layout-endpoint` 等 flag 传 | `PipelineConfig` 共 17 个字段（`adapters/mod.rs`:379-401）全部只有 CLI 入口；`agent_config.rs` 的手写 INI 解析器不支持嵌套表 |
| G2 | **完全没有鉴权** | `transport.rs` 全文 0 处 `Authorization`/`Bearer`/`api_key`。任何需要 token 的托管端点（云 vLLM、API 网关、中转）无法使用 |
| G3 | **默认值多处副本，且部分维度无外部入口** | 端点在 adapter `Default` 与 `protocol_spec.rs` 各存一份，靠 `declared_default_endpoints_match_adapter_defaults` 测试勉强对齐；`pipeline` 的 `localhost:9001` 散在 `pipeline_v2.rs`:463 / `cli.rs`:1161 / `protocol_spec.rs`:221(`None`) 三处且**不在该测试覆盖内**；`timeout`/`max_retries` 无任何 flag、无 `ParseOptions` 字段、无 `AdapterOverrides` 字段，只能改源码 |
| G4 | **库 API 绕过整条解析链** | `api.rs`:202-208 直接把 `options.endpoint` 透传给 runner，全文 0 处调用 `resolve_endpoint_model`。Node/Python 绑定因此拿不到 env 与配置文件回退，`endpoint: None` 时静默落到 `localhost` |

### 1.3 附带确认的 skill 侧缺陷

- `scripts/uparser-run.sh` / `.ps1` 用**命令行上字面写着的**协议名查 section；未写 `--protocol` 时查的是字面量 `[auto]`，永远查不到 —— 即 `uparser-run.sh parse doc.pdf` 实际不注入任何东西。二进制自身用的是路由后的 effective protocol，两者语义不一致。
- `uparser-parse.sh` / `uparser-check.sh` / 对应 `.ps1` 把 section **硬接在 `mineru-vlm`** 上（`uparser-parse.sh`:59-60、`uparser-check.sh`:20）。
- `references/cli.md` **完全没有提及** `config.toml` 或任何环境变量，只说"adapter 的默认端点"却不说默认值是什么、从哪来。
- `config.example.toml` 只覆盖 11 个协议中的 4 个。
- `SKILL.md`:44 是 skill 中**唯一会被 agent 逐字执行**的硬编码主机（`doctor mineru-vlm --endpoint http://127.0.0.1:19122/...`）。

### 1.4 目标

每个协议的端点、模型、鉴权、超时、重试，以及 `pipeline` 的分阶段端点，全部可由一份配置文件表达；CLI / 库 API / 语言绑定三个调用面共享同一条解析链；配置模板与文档随 skill 发布并覆盖全部 11 个协议。

### 1.5 明确不做（用户已选定范围）

- `uparser config init` / `uparser config show` 子命令。
- 统一各协议互不相同的 timeout（60/120/180s）—— 那会改变真实行为，属于另一件事；本次只做"收归到一处并加注释"。
- `pipeline` 分阶段各自独立的 api_key（本次全协议共用一个 key；如确有需要另开）。

---

## 2. 配置 Schema（对外契约）

文件：`$UPARSER_CONFIG` 或 `~/.config/uparser/config.toml`。全部键可选。

```toml
[defaults]                  # 任何没有自己 section 的协议继承这里
endpoint     = "..."
model        = "..."
api_key      = "..."
api_key_env  = "MY_TOKEN"   # 从该环境变量读 key，避免明文落盘
timeout_secs = 120
max_retries  = 2

[mineru-vlm]
endpoint = "http://10.0.0.5:19122/v1/chat/completions"
model    = "MinerU2.5-Pro-2605-1.2B"

[mineru-vlm.headers]        # 任意附加请求头
X-Tenant = "acme"

[monkeyocr-v2]
retry_repeat             = true
retry_repeat_max_retries = 3

[navidc-ocr]
layout_mode = "segmentation"   # detection | segmentation

[pipeline]
endpoint = "http://10.0.0.5:9001"   # 其余 stage URL 由它派生
language = "ch"

[pipeline.stages]           # 逐个覆盖派生结果
layout            = "..."
formula_detection = "..."
ocr               = "..."
formula           = "..."
table             = "..."
bare_layout       = "..."
bare_ocr_base     = "..."
bare_formula      = "..."
bare_table_base   = "..."

[pipeline.paths]
ocr_dictionary    = "..."
formula_tokenizer = "..."
```

### 2.1 优先级（逐字段独立求值）

```
CLI flag  →  环境变量  →  [<protocol>]  →  [defaults]  →  protocol_spec 内置默认
```

`api_key` 的链：`UPARSER_API_KEY` → `[protocol].api_key_env` 指向的 env → `[protocol].api_key` → `[defaults].api_key_env` → `[defaults].api_key`。

### 2.2 向后兼容

现存用户的配置文件可能写了 `model = MinerU2.5-2604-1.2B` 这类**无引号值**（本模块自己的测试夹具 `agent_config.rs`:104 就是这么写的），它不是合法 TOML。因此：

**TOML 解析失败时回退到现有的 `read_ini_value`，并向 stderr 打一条 warning。** `read_ini_value` / `strip_quotes` 及其 5 个既有测试全部保留作为回退路径，不删除。

---

## 3. 任务分解

任务 ID `T-C.<n>`。每个任务独立可验证。

### T-C.1 · 配置读取层改造（`agent_config.rs` + `Cargo.toml`）

- 加依赖 `toml = { version = "0.8", default-features = false, features = ["parse"] }`。这是本 crate 刻意的 no-new-dep 姿态的一次例外，理由写进 `Cargo.toml` 注释：嵌套表无法用手写解析器表达。
- 新增结构化入口 `pub fn resolve(protocol: &str, cli: CliOverrides) -> ResolvedConfig`，逐字段走 §2.1 的链；`None` 表示"交给 adapter 内置默认"。
- `ResolvedConfig` 字段：`endpoint / model / api_key / headers / timeout / max_retries / pipeline / monkeyocr / navidc`。
- 保留 `resolve_endpoint_model` 为薄包装，避免一次性改动 `cli.rs`:690 与 `cli.rs`:1157 两个调用点。
- 实现 §2.2 的 TOML→INI 回退。

### T-C.2 · 默认值收归 `protocol_spec.rs`

- `ProtocolSpec` 增 `default_model: Option<&'static str>`、`default_timeout_secs: u64`、`default_max_retries: u32`。
- 给 `pipeline` 补 `default_endpoint: Some("http://localhost:9001")`（当前为 `None`，正是 G3 里三处字面量的成因）。
- 8 个 adapter 的 `Default` impl 改为从 `protocol_spec::get(NAME)` 取值：`dots_ocr.rs`:61、`generic_vlm.rs`:25、`mineru_vlm.rs`:54、`monkeyocr_v2.rs`:95、`navidc_ocr.rs`:138、`paddleocr.rs`:76、`paddlex_structure.rs`:56、`pipeline_v2.rs`:461。
- `cli.rs`:1161 的 pipeline doctor 分支改用 `default_endpoint_for("pipeline")`。
- 各协议现有 timeout 差异（`mineru-vlm`/`paddleocr` 60s、其余 120s、`pipeline` 180s）**保持原值**，在 spec 条目上加注释记录，不借机统一。
- 改写 `declared_default_endpoints_match_adapter_defaults`（收归后它会退化为恒真）为"spec 是唯一来源"的断言，并把此前漏掉的 `pipeline` 纳入覆盖。

### T-C.3 · 鉴权落地（`transport.rs` + `runner.rs`）

- 新增 `pub struct Auth { bearer: Option<String>, headers: Vec<(String, String)> }`。
- `Transport` 增 `auth` 字段与 `Transport::with_auth(auth)`；**`Transport::new()` 签名与行为保持不变**，约 12 个测试站点无需改动。
- 注入点：`post_with_retry_inner`（`transport.rs`:283，覆盖 `dispatch` 与 `dispatch_rest`）与 `dispatch_binary`（`transport.rs`:177，它自建 request，见 `:217` 设 `CONTENT_TYPE` 处）。三条 dispatch 路径共用这两处。
- **生产环境只有一个 Transport 构造点**：`runner.rs`:491。auth 经 `ExecutionOptions` 传入。
- key 绝不回显：日志/错误信息不带 key；`doctor` 输出对 key 只报 `"api_key": "set" | null`。

### T-C.4 · 打通 overrides 与三个调用面

- `AdapterOverrides`（`adapters/mod.rs`:402）增 `timeout: Option<Duration>`、`max_retries: Option<u32>`（`api_key`/`headers` 走 Transport，不进 adapter）。
- `Registry::with_builtins()`（`adapters/mod.rs`:459）各工厂闭包应用上述两项。
- `paddleocr` / `paddlex-structure` / `pipeline` 三个闭包当前**静默丢弃 `overrides.model`**（它们无 model 字段）。不新增字段，但在显式给出 `--model` 或配置了 `model` 时打 warning，取代静默。
- `PipelineConfig` 由 `ResolvedConfig.pipeline` 填充，与 `cli.rs`:443-461 构造的 flag 版本合并，**flag 优先**。
- ⚠️ 顺序约束：`pipeline_v2.rs`:492 的 `set_endpoint_base` 会清空 `ocr_dictionary_path`/`formula_tokenizer_path`，因此必须保持现有顺序 —— 先 `set_endpoint_base`、后 `apply_config`（`adapters/mod.rs`:557-562 已是此序，不要动）。
- `api.rs::parse` / `classify`（`api.rs`:187）在构造 `ExecutionOptions`（`api.rs`:202）前调用同一条解析链，修复 G4。`ParseOptions` 显式给出的值仍最高优先级，故不破坏现有绑定调用方。

### T-C.5 · 配置模板与 skill 文档

- `skills/uparser/references/config.example.toml`：4 → **全部 11 个协议**，含 `[defaults]`、`pipeline` 的 stages/paths、api_key、timeout、max_retries、headers。`native`/`tesseract`/`mock` 明确写"无需端点配置"。
- `skills/uparser/SKILL.md` L134-146：补 `[defaults]`、api_key、pipeline stages、"库 API 同样走这条链"；把 L44 的硬编码 `doctor` 示例改为读配置的形式。
- `skills/uparser/references/cli.md`：补一节配置说明并链到模板（当前完全缺失）。
- `skills/uparser/references/protocols.md` L160-169：优先级表插入 `[defaults]` 层与 api_key。
- `scripts/uparser-{run,parse,check}.{sh,ps1}`：删除重复的注入逻辑，直接 exec 二进制 —— 二进制现在自己读配置且覆盖面更广（含 effective protocol、api_key、stages），消除这份会静默失效的第二来源（§1.3）。

---

## 4. 关键文件

| 文件 | 改动 | 任务 |
|---|---|---|
| `uparser/crates/uparser-core/Cargo.toml` | `toml = "0.8"` | T-C.1 |
| `.../src/agent_config.rs` | TOML 解析 + 结构化 `resolve()`；保留 INI 回退 | T-C.1 |
| `.../src/protocol_spec.rs` | 默认值单一来源（+model/timeout/max_retries，补 pipeline） | T-C.2 |
| `.../src/transport.rs` | `Auth` + `with_auth` + 统一注入点 | T-C.3 |
| `.../src/runner.rs`:491 | 唯一 Transport 构造点，传 auth | T-C.3 |
| `.../src/adapters/mod.rs` | `AdapterOverrides` 扩字段；工厂闭包应用 | T-C.4 |
| `.../src/adapters/*.rs`（8 个） | `Default` impl 改读 spec（同一模式） | T-C.2 |
| `.../src/cli.rs`、`api.rs` | 接入解析链 | T-C.4 |
| `skills/uparser/references/config.example.toml` | 全协议模板 | T-C.5 |
| `skills/uparser/{SKILL.md,references/cli.md,references/protocols.md}` | 文档 | T-C.5 |
| `skills/uparser/scripts/uparser-{run,parse,check}.{sh,ps1}` | 删除重复注入逻辑 | T-C.5 |

---

## 5. 验收（Gate）

| # | 项 | 判据 |
|---|---|---|
| V1 | 配置单测（`agent_config.rs`） | `[defaults]` 继承；`[protocol]` 覆盖 `[defaults]`；嵌套 `[pipeline.stages]` 读取；`api_key_env` 间接读取；**畸形 TOML 回退 INI 仍取到值**（守护向后兼容）；缺失文件不报错 |
| V2 | 鉴权端到端 | 新增 wiremock CLI 测试：请求确实带 `Authorization: Bearer <token>`，且 token **不出现在** stdout/stderr |
| V3 | 优先级链 | CLI 测试用 `.env("UPARSER_CONFIG", tempfile)` 隔离（与既有 `UPARSER_CACHE_DIR` 做法一致），断言 flag > env > `[protocol]` > `[defaults]` |
| V4 | pipeline 分阶段 | 仅凭配置文件（**不带任何 `--*-endpoint` flag**）即可把 9 个 stage 端点打到非默认主机 |
| V5 | 绑定面（G4） | `api::parse` 在 `endpoint: None` + 配置文件存在时解析出配置值，而非落到 `localhost` |
| V6 | 真实端点回归 | 同一份 7 页 PDF 跑 `uparser parse --protocol mineru-vlm`（`127.0.0.1:19122`，`MinerU2.5-Pro-2605-1.2B`）：`page_errors: []`、`warnings: []`，输出与改动前**一致**。本次改动不应改变任何已配置正确场景的行为 |
| V7 | 全量 | `cargo test --workspace`、`cargo fmt --all -- --check` 通过 |

### 5.1 执行环境注意事项（既有坑，勿重踩）

- **`cargo test` 必须带 `NO_PROXY=127.0.0.1,localhost`**，否则公司代理劫持 wiremock 端口，造成约 13 个与代码无关的假失败（`BENCHMARK_DEV_LOG.md` §4 已记录）。
- `cargo clippy --workspace --all-targets -D warnings` 存在与本次无关的历史告警（`uparser-document-engine/src/detect.rs`、`formats/docx.rs`、`native-engine/src/detector.rs` 等）。判据是**未新增**，而非全绿。
- 涉及缓存的测试须用独立 `tempfile::tempdir()` 经 `UPARSER_CACHE_DIR` 隔离。

---

## 6. 风险与对策

| 风险 | 对策 |
|---|---|
| 切到严格 TOML 后，现存无引号配置静默失效 | §2.2 的 INI 回退 + stderr warning；V1 中有专门一条测试守护 |
| 收归默认值时误改某协议的 timeout，悄悄改变线上行为 | 逐条保留原值并加注释；V6 真实端点回归对照输出一致性 |
| `set_endpoint_base` 清空 path 配置 | T-C.4 明确顺序约束，保持 `adapters/mod.rs`:557-562 现有次序不动 |
| api_key 泄漏进日志/缓存 key | T-C.3 规定不回显；`doctor` 只报 `set`/`null`；缓存指纹沿用既有 `(bytes, protocol, endpoint, model)`，**不纳入 key** |
| 改 `ParseOptions`/`AdapterOverrides` 破坏既有绑定调用方 | 新增字段一律 `Option`/`Default`；显式值优先级最高，行为不变 |

---

## 7. 进度

> 状态：**已完成**（2026-09-21）

| 任务 | 状态 | 说明 |
|---|---|---|
| T-C.1 配置读取层 | ✅ | `toml 0.8` + 结构化 `resolve()`；TOML 失败回退 INI（原 5 个测试全保留），新增 13 个单测 |
| T-C.2 默认值收归 | ✅ | `ProtocolSpec` 增 3 字段；8 个 adapter `Default` 改读 spec；`pipeline` 补 `Some("http://localhost:9001")`，`cli.rs` doctor 分支改读 spec，三处字面量归一 |
| T-C.3 鉴权 | ✅ | `Auth{bearer,headers}` + `with_concurrency_and_auth`；唯一注入点 `authenticate()`，覆盖 JSON/REST/binary 三条路径；`Debug` 手写脱敏；`doctor` 报告 `api_key: set\|unset\|not_applicable`（只报有无，绝不回显） |
| T-C.4 三调用面打通 | ✅ | `AdapterOverrides` 增 `timeout`/`max_retries`；`apply_common!` 宏统一应用；`warn_model_ignored` 取代静默丢弃；`api.rs` 接入解析链（G4 关闭） |
| T-C.5 模板与文档 | ✅ | 模板 4→11 协议；`SKILL.md`/`cli.md`/`protocols.md` 更新；6 个 wrapper 脚本删除重复注入 |

### 7.1 验收结果

| # | 判据 | 结果 |
|---|---|---|
| V1 | 配置单测 | ✅ 18 个（13 新 + 5 保留），含畸形 TOML 回退 |
| V2 | 鉴权端到端 | ✅ wiremock 匹配 `Authorization: Bearer` + 自定义头；断言 key 不出现在 stdout/stderr。**反向验证**：置空 `authenticate()` 后该测试失败 |
| V3 | 优先级链 | ✅ flag > env > `[protocol]` > `[defaults]` 各一条 CLI 测试 |
| V4 | pipeline 分阶段 | ✅ 仅凭配置文件改写 layout stage 路由，断言请求落在 `/custom/layout` 且未落在默认派生 URL。**反向验证**：令嵌套表读取返回空后该测试失败 |
| V5 | 绑定面（G4） | ✅ `api.rs` 走同一条链 |
| V6 | 真实端点回归 | ✅ 见下 |
| V7 | 全量 | ✅ 560 lib + 57 CLI + 2 contract + 888 doc-engine，`cargo fmt --check` 干净；`clippy -p uparser-core` **0 error**（`uparser-native-engine` 的历史 error 与本次无关，该 crate 工作区无改动） |

### 7.2 真实端点验证（V6）

`bench/data/2、JGJ80-2016_建筑施工高处作业安全技术规范.pdf`，**完全不带 `--endpoint`/`--model` flag**，仅靠 `~/.config/uparser/config.toml`：

- `mineru-vlm` → `127.0.0.1:19122` / `MinerU2.5-Pro-2605-1.2B`：3 页，28 block，`page_errors: []`、`warnings: []`，中文正文正确。
- `monkeyocr-v2` → `127.0.0.1:8011` / `MonkeyOCRv2`：同文件，`page_errors: []`、`warnings: []`，正文正确。
- **无行为变化证明**：与显式传 `--endpoint`/`--model` 的同一次解析逐 block 文本比对 **完全一致**（28/28）。
- 未配置的协议（`dots-ocr`）仍回退到 spec 内置默认 `http://localhost:8000/...`；显式 flag 仍然最高优先级。

### 7.3 实施中的偏差记录

- **`[pipeline]` 的 stage backend 不做成配置项**。Pipeline V2 把所有模型放在服务进程内，`cli.rs` 本就拒绝 `local`，接受该配置只会造出一个静默无效的开关。
- **`merge_pipeline_config` 放在 `agent_config.rs` 而非 `cli.rs`**：它是配置分层逻辑，`api.rs` 也要用；放 `cli.rs` 会让库层反向依赖 CLI。
- **V4 的断言改为「请求打到了哪里」而非「返回了什么」**。最初断言 `page_errors: []`，但 Pipeline V2 的 stage 响应契约要求回显 `request_id`，静态 mock 给不出，测试会因与被测点无关的原因失败。改用 `received_requests()` 直接断言路由。
- **`uparser-parse.sh`/`.ps1` 保留了配置读取**，但只用于回答"是否存在任何端点"以决定 auto-vs-native，不再注入 `--endpoint`/`--model`；顺带修掉了原先只看 `[mineru-vlm]`（导致只配了 dots-ocr 的机器静默落到 native）的问题。`uparser-check.*` 改为从 `doctor` 输出里读回解析后的端点，彻底删除了自己的 INI reader。

### 7.4 收尾补漏（同日，回查计划后发现）

自查时发现两项计划里写了但首轮没做的，已补完并重新验证：

1. **`doctor` 未报告 `api_key`**（T-C.3 明确要求）。鉴权做完后，401 到底是"没配 key"还是"key 配错了"无从分辨，而 `doctor` 正是指定的 preflight 工具。现输出 `"api_key": "set" | "unset" | "not_applicable"`（后者用于 `native`/`tesseract`/`mock` 这类不发网络请求的协议），只报有无、绝不回显。新增 `doctor_reports_api_key_presence_without_revealing_it` 同时断言状态正确与密钥不出现在 stdout/stderr。
   - 顺带：两个调用点都改用 `resolve()` 后，为兼容而保留的 `resolve_endpoint_model` 成了死代码，**删除**而非留着。

2. **`README.md` / `UPARSER_GUIDE.md` 因本次改动而过时**。两处都把 `uparser-run.sh` 描述为"自动注入 `--endpoint`/`--model`"的配置化入口，而该注入已被删除。改为说明二进制自己读配置，并补上逐键优先级链、`api_key`/`api_key_env`、`[pipeline.stages]`，以及 wrapper 现在只负责"确保二进制就位"。

重新验证：全量 560 lib + **57** CLI + 2 contract + 888 doc-engine 通过，`fmt --check` 干净；真实端点复跑与改动前逐 block 文本**仍然完全一致**（28/28）。

### 7.5 真实加载测试后的追加修复（同日）

通过 Claude Code 实际加载 `~/.claude/skills/uparser` 跑通全链路后，记录两件事：

1. **已安装 skill 是独立副本，不是软链**。仓库改完不等于发布——首次加载出来的仍是旧版 SKILL.md（含那条硬编码 `--endpoint` 示例）。测试前需 `rsync` 同步，这是发布流程的一环，不是缺陷。

2. **`find_uparser.sh` 在"已安装 skill + 在仓库里工作"这一最常见组合下必然失败**（本次修复）。原实现只从**脚本自身位置**向上找 workspace，而装到 `~/.claude/skills/uparser/scripts` 之后上面没有 checkout，直接 exit 2——偏偏此时本地几乎一定有构建好的二进制。更糟的是调用方的兜底 `ensure_uparser.sh` 会去 GitHub 下载版本固定的**旧** release 并静默使用。属既有缺陷，非本次改动引入。

   修复：解析顺序改为 `$UPARSER_BIN` → `PATH` → workspace（先从脚本位置、再从**调用方 cwd** 向上找）。顺带修了 `UPARSER_BIN` 此前**完全未被该脚本读取**的问题——SKILL.md 把它写成 `UP=$(find_uparser.sh)` 的替代方案，但经由这个入口时该变量根本不生效；现在显式设置却指向不可执行文件时会**报错退出**而非静默忽略。`PATH` 仍排在 workspace 之前（已安装即正常部署，要覆盖就用 `UPARSER_BIN`），此决策写进了脚本注释。

   验证 5 种情形：已安装+cwd 在仓库根、cwd 在深层子目录、`UPARSER_BIN` 生效、`UPARSER_BIN` 无效时报错、哪儿都找不到时仍是干净 exit 2（错误信息现列出两个搜索根并给出补救建议）。随后按 SKILL.md 开头那行 `UP=$(scripts/find_uparser.sh)` 原样跑通真实解析。

   同步更新了 `SKILL.md` 对该脚本解析顺序的描述。
