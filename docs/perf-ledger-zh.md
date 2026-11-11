# 性能优化账本

> 性能优化账本：cc-switch 的性能优化以此文件为唯一台账。
>
> 规则：每一个待优化项先在这里排队（带证据），落地后把实测数据补进来再改成「已完成」；
> 被证伪的做法直接删掉，不留传说。测量命令统一收在
> [`src-tauri/src/proxy/forwarder.rs`](src-tauri/src/proxy/forwarder.rs) 的 `perf_probe` 模块里。

## 审计基线与方法

基线提交：`150d9b1c`（Merge upstream/main 之后，v4.0.33）。审计范围是 `src/`（508 个 TS/TSX、
5.71 MB）与 `src-tauri/src/`（378 个 .rs、13.6 MB），重点放在代理热路径：
请求进 `handle_messages_for_app` → `RequestContext::new` → `forward_with_retry` → `forward`，
出站经 `serialize_request_body` / `hyper_client`，回包经 `response_processor`。

测试基线（改动前全绿）：

- 后端 `cargo test --lib`：5005 passed / 0 failed / 17 ignored
- 后端集成测试 `cargo test --tests`：218 passed / 0 failed
- 前端 `pnpm vitest run`：4032 passed / 1 failed（`tests/integration/App.test.tsx` 一例 10s 超时，
  同文件其余用例本身就要 8~15s，属该文件的执行时长噪声，非逻辑失败）
- `pnpm typecheck`、`pnpm build:renderer`（20~27s）通过
- `cargo clippy --all-targets --all-features`：0 warning
- 写稿时全仓 378 个 .rs 里 53 个不写内联测试，多为命令薄封装或纯测试文件

### 测量方法与噪声

`cargo test --lib --release perf_probe -- --ignored --nocapture` 在同一进程里量四段：
请求体深拷贝、私有字段只读扫描、序列化、经 loopback 假上游的完整转发；`canonical_json_string_costs`
另做新旧两条路径的同体对照。

⚠️ 机器噪声比多数待优化项的收益还大：两轮间隔测量里，没有改动的 `value deep clone`
读数在 0.141ms / 0.119ms 之间晃，`full forward, pure passthrough` 在 0.923ms / 1.021ms
之间晃（±10%），并行跑别的东西会更差。**结论一律以同一进程内的对照为准**，
跨轮单测的数字只当量级参考。前端包体数字以 `pnpm build:renderer` 的输出为准（稳定）。

## 待优化项与状态

### P0-1 规范化每个字符串/键都做一次堆分配

- 位置：[`src-tauri/src/proxy/json_canonical.rs`](src-tauri/src/proxy/json_canonical.rs) `write_canonical`
- 现状：字符串和对象键各走一次 `serde_json::to_string` 再 `push_str`，等于每个字符串
  一次分配 + 一次整段拷贝；对象还要为键排序临时 `Vec` 分配一次。
- 影响面：`canonical_json_string` / `canonicalize_tool_arguments_str` 在 tool_media、
  transform_codex_chat、transform_codex_anthropic、transform_responses、streaming_codex_*
  里每个 SSE 事件 / 每个工具调用都要过一次。
- 证据一（分配次数，线程本地计数分配器）：同一份 8 字符串体 vs 160 字符串体，
  改造前 48 → 775 次（随字符串数线性增长），改造后只随输出缓冲翻倍增长，
  160 字符串那档和 8 字符串那档只差 ≤ 8 次。
- 证据二（同进程同体 A/B，`cargo test --lib --release perf_probe -- --ignored --nocapture`，
  901 个字符串、2000 轮、两侧输出逐字节一致由测试断言）：

  | 请求体 | 改造前 | 改造后 | 降幅 |
  | --- | --- | --- | --- |
  | 全部无转义 | 167.87 µs | 50.95 µs | **-69.6%** |
  | 一半带引号/反斜杠 | 180.75 µs | 43.69 µs | **-75.8%** |

- 状态：**已完成**
- 测试：`write_json_string_matches_serde_json_escaping`（与 serde_json 逐字节对照，
  覆盖全部控制字符码位 + DEL + 非 ASCII）、`canonicalizing_does_not_allocate_per_string_or_key`
  （分配计数不随字符串数线性增长）、原有 7 个规范化测试不变

### P0-2 首屏包被 Copilot/Pi 预设目录污染

- 现状：`ProviderCard.tsx` 只为判断「这张卡是不是 BYOK」引 `isCopilotByok`，
  而它在 `copilotProviderPresets.ts` 里，该文件 `import { piProviderPresets }` 整份目录
  （113 KB）并派生出 Copilot 目录。入口块因此拖着这两份数据。
- 证据（`pnpm build:renderer`，入口块）：

  | 指标 | 改前 | 改后 |
  | --- | --- | --- |
  | `main-*.js` 体积 | 743.31 kB | 655.20 kB |
  | gzip | 224.20 kB | 210.80 kB |
  | 入口静态图模块数 / 源码量 | 222 个 / 2.90 MB | 192 个 / 2.22 MB |

- 位置：[`src/config/copilotEnv.ts`](src/config/copilotEnv.ts)（新，环境变量键表 + 格式 + env 助手，
  一条预设目录的值导入都没有）、[`src/config/copilotProviderPresets.ts`](src/config/copilotProviderPresets.ts)
  （只剩目录数据）
- 状态：**已完成**
- 测试：`src/config/copilotProviderPresets.test.ts` 拆成两个来源后仍 4 passed；
  `pnpm typecheck` 通过

### P0-3 SSE 块扫描有三份同构实现，热路径用了会分配的那个

- 位置：[`src-tauri/src/proxy/sse.rs`](src-tauri/src/proxy/sse.rs) `scan_sse_block` /
  `scan_sse_block_ref` / `sse_block_data`
- 现状：`scan_sse_block` 为事件名分配 `String`，`scan_sse_block_ref` 与之逐行同形但借用；
  `streaming_responses.rs`、`transform_codex_responses_xai_sanitize.rs`、
  `responses_late_arguments.rs` 用的都是分配版本，每个 SSE 事件白付一次 String。
- 处理：两实现合并为一个借用的 `scan_sse_block`，删掉 `scan_sse_block_ref`；
  需要 owned 的 `responses_late_arguments`（块要活到重放）就地 `.map(str::to_string)`。
- 状态：**已完成**
- 测试：`sse.rs` 原有 SSE 扫描测试未改断言（`event.as_deref()` 两种形状都吃）；
  `cargo test --lib` 全量通过

### P0-4 xAI 原生流式整流每个块两次分配两次拷贝

- 位置：[`src-tauri/src/proxy/providers/transform_codex_responses_xai_sanitize.rs`](src-tauri/src/proxy/providers/transform_codex_responses_xai_sanitize.rs) `rewrite_xai_native_sse_block`
- 现状：四条原样回吐分支都是 `Bytes::from(format!("{block}\n\n"))`——先建 String 再整段
  拷进 Bytes。完全没有 namespace 要还原的块（多数）也照付。
- 处理：按 `block.len() + 2` 一次分配，`extend_from_slice` 两次写。
- 状态：**已完成**
- 测试：该文件原有 SSE 整流测试通过（输出字节不变）

### P0-5 无条件求值的 `unwrap_or` / `or` / `map_or` / `or_insert`

### P0-6 测试代码里 80 处多余 `.clone()`

- 现状：`let mut x = y.clone()` 而后文只用 x、`Arc::new(ProviderRouter::new(db.clone()))`
  之类——克隆出来的那份原值再没被读过。扫描手段同 P0-5：
  `cargo clippy --lib --tests -- -W clippy::redundant_clone`，命中 160 条、
  分布在 35 个文件，全部落在 `#[cfg(test)] mod tests` 或 `src-tauri/tests/` 里
  （`cargo clippy --lib` 不带 `--tests` 时是 0 条，所以生产代码本来就没有）。
- 处理：`cargo clippy --fix --lib --tests --allow-dirty --allow-staged --
  -W clippy::redundant_clone` 按 clippy 自己的 suggestion 删，79 处；
  diff 逐条审过，全是删除 `.clone()` 的等价替换。
- 状态：**已完成**
- 测试：`cargo test --lib` 5007 passed / 0 failed、`cargo test --tests` 5225 passed /
  0 failed，与改前同一数字；默认 lint 的 clippy 0 warning；`cargo fmt --check` 干净

### 踩坑：`clippy --fix` 一次只修一类 lint，删掉 `.clone()` 会生出下一条 warning

删掉 `db.clone()` 后，`SourceLocation::Sqlite { db: db, ... }` 里的字段名和变量名
变得同名，于是 `redundant_field_names`（默认 lint，CI 挂着 `-D warnings`）冒出来。
`--fix` 一轮不会跟进修它。所以跑 `--fix` 之后**必须再跑一次默认集的
`cargo clippy --lib --all-targets`**，把这类次生 warning 一次清掉，否则 CI 会挂
在这一条上。本轮就是这样抓到并改成 `db,` 的。

- 位置：`copilot_auth.rs`、`omp_config.rs`、`hermes_config.rs`、`dsh_config.rs`（2 处）、
  `claude_mcp.rs`、`live/patch/mod.rs`、`commands/misc.rs`、`services/subscription.rs`、
  `session_manager/providers/{codex,claude,copilot}.rs`（4 处）、
  `session_manager/providers/gemini.rs`
- 现状：`Option::unwrap_or(f)` / `or(f)` / `map_or(f, _)` / `Entry::or_insert(f)` 的**参数
  是无条件求值**的——`Some` 分支也要为那个 `f` 付一次代价。扫描手段：
  `cargo clippy --lib -- -W clippy::or_fun_call -W clippy::needless_collect
  -W clippy::redundant_clone -W clippy::map_clone -W clippy::useless_vec
  -W clippy::boxed_local`（默认 lint 集不含这些 nursery 项，所以平时是静音的）。
- 其中两处不只是分配，是真实的多余工作：
  - `live/patch/mod.rs:225`：`rfind('\n').map_or(before.chars().count(), ...)`——
    找到换行时也白把整段前缀数一遍字符（O(n)）。live patch 每次写入都走这里。
  - `session_manager/providers/codex.rs:1636/1690`：
    `command.cwd.clone().or(detail.take())`——左支有 cwd 时 `detail.take()` 也已执行，
    `detail` 被置空。此处 `detail` 之后只被赋值不再被读，改惰性求值后终值一致。
- 处理：一律换成 `unwrap_or_else` / `or_else` / `map_or_else` / `or_insert_with`，
  或去掉多余的中间 `collect`。
- 状态：**已完成**
- 测试：`cargo test --lib` 5007 passed / 0 failed（改前改后同为 5007，无行为差异）；
  默认 lint 的 clippy 仍 0 warning；nursery 这组从 24 条降到 8 条
- 剩下 8 条判定不改并写明原因：`uri.path()`、常量三元、`ip.into()`、Option 的
  `as_ref()` 这些参数本身免费，套一层闭包只会把可读性弄差；
  `services/skill.rs:2382` 的 `or_insert(UnmanagedSkill{..})` 要把三个局部 move 进
  闭包，被迫提前 clone 一次 `label`，反而在 and_modify 分支上多一次 String 克隆。

### P1-1 每个代理请求重取一次主目录

- 现状：`handler_context.rs` 的 `DeviceStore::for_device()` 每请求调
  `config::get_home_dir()`：`std::env::var_os` + `CC_SWITCH_TEST_HOME` 解析 + `PathBuf`。
- 实测（`perf_probe::home_dir_costs`；release 与 debug 两轮，比例一致）：

  | 环节 | 耗时 |
  | --- | --- |
  | `std::env::var_os("CC_SWITCH_TEST_HOME")` | ~2.35 µs |
  | `dirs::home_dir()` | ~0.55 µs |
  | `config::get_home_dir()` | ~2.8 µs |
  | `DeviceStore::for_device()` | ~3.0 µs |

  **大头是环境变量读本身（约 70%），不是 `SHGetKnownFolderPath`。**
- 结论：**不动**。要把这 2.35 µs 省掉，缓存就不能每请求读环境变量；而
  `CC_SWITCH_TEST_HOME` 一变更就得失效，全仓有 37 个文件会改它（`TestHomeScope`
  之外还有直接 `env::set_var` 的）。为 0.3% 的单请求收益引入「缓存不同步 = 测试
  写错目录」的风险不划算。剩下的 0.65 µs 缓存下来也看不出差别。
- 若将来要把入口侧的 `DeviceStore` 换成进程级实例，正确做法是让
  `config::get_home_dir()` 自己带缓存，并由那个环境变量的唯一写入口负责作废。

 ### P1-2 改写出站体时的序列化与深拷贝

### P1-2 改写出站体时的序列化与深拷贝

- 实测（release，200 轮，422 KB 请求体；三轮读数 `value deep clone` 0.141 / 0.119 / 0.091 ms，
   `serialize to bytes` 0.341 / 0.392 / 0.385 ms，`parse response json` 0.135 / 0.152 / 0.145 ms，
   `full forward, pure passthrough` 0.923 / 1.021 / 0.904 ms）：
  量级为序列化一次 0.3~0.4 ms、深拷贝 0.1 ms、整包解析 0.14 ms、纯透传整条转发 0.9~1.0 ms。
- 纯透传已经零深拷贝 + 复用客户端原始字节（`RawRequestBody`），没有明显可省；
  改写路径的 0.34 ms 里 `serde_json` 的 IndexMap（`preserve_order`）解析/序列化是主项，
  换掉 `preserve_order` 会改变出站键序，风险高于收益，暂不动。
- 状态：**已评估，不动**

### P2 结构项（影响面大，需单独排期）

#### 审计后否定：配置模块族不是同构重复

- 账本原先把「claude / codex / dsh / hermes / kimi / mcode / omp / openclaw / opencode /
  pi / agy / copilot / gemini / grok / claude_desktop 十几份近同构读写处理器」
  列为待办。先取证再决定：15 个 `*_config.rs` 合计 23,457 行
  （codex 6 599、claude_desktop 3 192、dsh 2 846、hermes 2 611…），
  12 个有 `fn` 的文件共 695 个函数名，**只有 3 个名字出现在 ≥6 个文件里，
  没有任何名字出现在 ≥8 个文件里**。
- 结论：**否定这条待办**。每个 app 的配置载体不同（TOML / JSON / JSONC /
  settings.json / 进程环境 / 多段嵌套表），函数名不重叠说明逻辑并不重复，
  强行抽公共层是在为 20% 的共性背 80% 的特例复杂度。这条从账本划掉。

#### 审计结论：TS 侧没有死导出

- 扫法：正则提 `src/**` 下全部 1,794 个导出符号名，
  `rg -w -c -- <name> <repo root>` 数全仓出现次数，`<= 1` 即「除定义处无人引用」。
- 结果：**0 个**。`AddProviderDialog`、`applyCopilotApiFormat`、`buildSwitchSections`
  这些看着像只在定义处出现的名字，实际上都被别处 import 了。
- 结论：TS 侧不需要做「删死代码」这一轮。扫法记在这里，下轮加新模块后可以重跑。

#### 审计结论：Rust 侧死代码已清零，另删掉一个空转的 `allow(dead_code)`

- 同法扫 Rust：正则提 `src/**` 下 3 063 个 `pub` / `pub(crate)` 顶层项，
  `rg -w -c` 数在 `src/` 内的出现次数，`<= 1` 即无人引用。
  得 7 个候选，逐个查清后**没有一个是真死的**：4 个名字带 `test_hook`
  （`set_pricing_model_source_test_hook` / `switch_provider_test_hook` /
  `get_pricing_model_source_test_hook` / `import_default_config_test_hook`），
  3 个是 `app_config.rs` 的 `get_manager` / `get_manager_mut` / `ensure_app`——
  后者只被 `src-tauri/tests/` 下的集成测试使用，所以 `src/` 内扫不到。
- 另外核了一遍 7 个**无条件** `#[allow(dead_code)]`（`rg '^\s*#\[allow\(dead_code\)\]'`）：
  6 个合理——`app_config.rs` 的 TempHome 字段靠 Drop 管临时目录生命周期、
  `omp_config.rs` 那个是测试专用 helper、其余是 TempHome 的 `dir` 字段；
  **1 个是空转的**：`proxy/error.rs` 的 `Internal(String)`，全仓 26 处构造，
   Variant 根本不 dead，`allow` 是 no-op。已删除。
- 结论：**Rust 与 TS 两侧的死代码都清零**，本条连同上面的 TS 扫描一起作为
  「无效代码」维度的收口记录。`cargo clippy --lib --all-targets` 0 warning、
  `cargo test --lib` 5 007 passed / 0 failed、`cargo fmt --check` 干净。

### P2-1 入口块拖着「应用」页的整套安装/卸载逻辑

- 位置：[`src/App.tsx`](src/App.tsx) → `src/components/apps/useToolUpdates.ts`（新）、
  [`src/components/apps/useToolManagement.ts`](src/components/apps/useToolManagement.ts)
- 现状：`App.tsx` 静态引入 `useToolManagement.ts`（1 088 行 / 42 KB）只为拿
  `checkToolUpdatesInBackground` 和 `useToolUpdatesAvailable` 两个小函数，
  整个「应用」页的安装 / 升级 / 卸载逻辑因此进了入口块。
- 处理：版本探测的 store（缓存、请求序号守卫、合并、TTL 判新鲜）连同
  `TOOL_NAMES` / `ToolName` / `ToolVersion` 一起拆到 `useToolUpdates.ts`——
  一条「应用」页逻辑的导入都没有；`useToolManagement.ts` 改成从它取，
  `toolVersionsCache` 从 `ToolManagementState` 里摘出去，版本监听和任务监听分开。
- 证据（`pnpm build:renderer`）：

  | 指标 | 拆前 | 拆后 |
  | --- | --- | --- |
  | `main-*.js` 体积 | 655.20 kB | 643.22 kB |
  | gzip | 210.80 kB | 206.95 kB |
  | 全量 JS 产物 | 6 761.9 kB | 6 761.8 kB |

  全量不变说明只是把代码搬进了懒加载块（版本搬运现在是 `AppsPage-*.js` 里的 40.2 kB），
  没有重复打包；入口块少 11.98 kB。
- 状态：**已完成**
- 测试：`tests/components/AppsPage.test.tsx` 58 个用例全过（含「重挂 + 过期缓存」
  这类直接压模块缓存的）；`tests/components/apps/*`、`npmBinCollision` 全过；
  typecheck + prettier 通过

### 第二轮改动一览

| # | 项 | 关键改动 | 验证 |
| --- | --- | --- | --- |
| 5 | P2-1 | 拆出 `useToolUpdates.ts`，入口块 -11.98 kB | AppsPage 58 用例 + 前端全量 |
| 6 | P1-1 | 量完判定「不动」，记在账本里 | `perf_probe::home_dir_costs` |

落地后复验（全部本地跑过，不依赖 CI）：

- `cargo test --lib`：5007 passed / 0 failed / 18 ignored（比基线多 2 个测试、1 个 ignored 探针）
- cargo test --tests（含 lib 单测目标，-j 2）：5 225 = 5 007 lib + 218 集成，全过
  （集成测试单独算是 218 个：16 个 tests/*.rs 文件，不重复数 lib 那 5 007 个）
- `cargo clippy --lib --all-targets`：0 warning
- `cargo fmt --check`：干净
- `pnpm typecheck`、`pnpm format:check`、`pnpm build:renderer`：通过
- `pnpm vitest run`：4032 passed；唯一失败的 `App.test.tsx` 用例单独跑通过（3.7s），
  是改动前就有的执行时长 flake，不是回归

⚠️ 本地跑 `cargo nextest run` 会因为同时构建 16 个集成测试二进制而撞上 Windows
提交上限（错误 1455「页面文件太小」）。加 `-j 2` 分段构建就正常，CI 上跑在 Linux
不存在这个问题。

### 第三轮：`forwarder.rs` 的 5,702 行测试搬去独立文件

- 位置：[`src-tauri/src/proxy/forwarder.rs`](src-tauri/src/proxy/forwarder.rs)（5,401 行）、
  新增 [`src-tauri/src/proxy/forwarder_tests.rs`](src-tauri/src/proxy/forwarder_tests.rs)（5,675 行）
- 动机：`forwarder.rs` 11,101 行里 **5,702 行（51%）是 `#[cfg(test)] mod tests`**，
  生产代码只有 5,401 行。仓库自己已经为这种体量的文件立了规矩——`database/dao/providers.rs`
  用 `#[path = "providers_tests.rs"] mod tests;`、`commands/misc.rs` / `proxy/session.rs` /
  `services/provider/mod.rs` 同样外挂测试文件；`forwarder.rs` 是唯一没跟上的大件。
- 处理：把 `mod tests { ... }` 整块搬进 `forwarder_tests.rs`，原位置只留
  `#[cfg(test)] #[path = "forwarder_tests.rs"] mod tests;`。模块名仍是 `tests`，
  所以测试全名（`proxy::forwarder::tests::...`）与 `use super::*` 的可见性一概不变。
  内含的 3 个嵌套 mod（`codex_stack` / `bookkeeping` / `perf_probe`）原样带走。
- 状态：**已完成**
- 验证：`cargo test --lib proxy::forwarder` 160 passed / 0 failed；
  全量 `cargo test --lib` **5 007 passed / 0 failed / 19 ignored**（与拆分前逐项一致）；
  `cargo test --tests` 5 225 = 5 007 lib + 218 集成，全过；
  `cargo clippy --lib --all-targets` 0 warning；`cargo fmt --check` 干净。
- 两个搬迁时踩到的坑（都只影响文件本身，未动测试逻辑）：
  1. **PowerShell 的 `[Collections.ArrayList]::new(数组)` 之后 `RemoveRange` 越界**：
     我按「总行数 - 起始下标 + 1」算长度，多算了一个，越界抛错但后续的 `InsertRange`
     仍然执行，文件被写成「声明 + 原 tests 块」并存。恢复用
     `git show HEAD:<path>` 重写文件，不要用 checkout。
  2. **仓库在 git 里是 LF，PowerShell 写文件默认 CRLF**：`Set-Content`/`WriteAllText`
     不指定就会写 CRLF，和 `.gitattributes` 的规范化冲突。搬完先把 `"`r`n"` 换成
     `"`n"` 再 `cargo fmt`，否则 fmt 会把整份文件重排一遍、diff 暴涨。

#### P2-2 图标表的 101 个内联 SVG 躺在「所有懒加载界面」的公共块里

- 证据（`build.sourcemap` 临时开一次，读 `disableContextMenu-*.map` 的 `sources`
  拿到精确的模块归属，分析完即关掉）：

  | 项 | 数 |
  | --- | --- |
  | 被懒加载块静态 import 的公共块 | `disableContextMenu-*.js` 566 kB，80 / 99 个 chunk 拖着它 |
  | 该块的源码构成 | 143 个源文件 = 67 node_modules + **54 个 `src/icons`** + 12 lib + 其余 9 |
  | `src/icons/extracted/index.ts` | 187 KB 源码里 **101 个内联 SVG 字面量共 178.8 KB** |

  公共块的 vendor 侧是 react-query（25 模块）、lucide（16）、react-i18next（7）、
  radix（2）这些真·公共底座，砍不动；app 侧 75 个源文件里 54 个是图标——
  也就是说这 566 kB 里约三分之一是一份「绝大多数界面根本不显示」的 SVG 字符串表。
- 谁在同步读它：`ProviderIcon`（渲染）、`ProviderIconBox` / `IconPicker` /
  `BasicFormFields`（元数据与衬底判断）。内联 SVG 的价值是 `currentColor` 继承
  文字色（`adaptSvgToTheme` 把近黑单墨图标改写成 currentColor），改成 `?url`
  资源文件会丢掉这个能力，不是纯搬移。
- 方案（要做就得做全套）：把 101 个 SVG 落成 `src/icons/extracted/generated/*.svg`
  独立文件，`icons` 换成 `import.meta.glob(..., { query: "?raw", eager: false })`，
  渲染路径改成按 key 取异步内容。附带能把 `needsLightBacking` 的「每次解析 SVG
  取色」固化成预计算名单（输入是固定的 101 + 52 个 key，输出必然固定）——
  这条顺带把每张卡 render 时的取色开销归零，且可以用「名单与运行时分析结果逐 key
  相等」的测试兜住。
- 状态：**待办**（收益已实测，见下；改动面大且收益薄，排在不改档）
- 补一次实测校准此前「1~3 ms」的估计：在 vitest（dev、未打包）里
  `await import("@/icons/extracted")` 20 次，**每次 74.57 ms**——但那个数里绝大部分是
  vite 对 154 个模块的逐个转换，不是生产成本。生产上这 180 KB 是那个 566 kB 共享块
  里的一段字符串字面量，chunk 下载后 V8 只需解析它一次，按 GB/s 量级折算约
  0.2~0.4 ms。判定不变：为一个 0.3 ms 的解析把 `ProviderIcon` 改成异步、还要核对
  currentColor 不退化，不划算。
- 附带把那条「顺带」也量了，免得下一轮再琢磨一遍：`needsLightBacking` 的取色判断
  热路径每轮（154 个 key 全查一遍）**0.041 ms**，冷路径（含 102 个 SVG 的解析与
  取色）是一次性的 1~5 ms 量级。把它固化成预计算名单只能省掉这两项里更小的那个，
  还要为一个 Map 换一个 Set —— 不做，已连同实测数一起记在这里。
- 顺带订正一条旧记录：`vite.config.ts` 的 `build.emptyOutDir` 不清 root 之外的
  目录（这里 root=`src`、outDir=`../dist`），跨实验的 chunk 文件会残留。凡是要
  「改动前 / 改动后」比字节的，必须先手工清空 dist 再构建，否则比的是两份不同
  的源码状态。上一版 manualChunks 的对比数就是这么错的，已按清空重测的数字订正。
- 状态：**待办**

### 图标表诊断（P2-2）与热路径取价量化

- 用 `build.sourcemap` 开一次（分析完即关）拿到精确的模块归属，结论见 P2-2：
  懒加载界面的公共块 566 kB 里 143 个源文件中 54 个是 `src/icons`，
  `index.ts` 187 KB 源码 = 101 个内联 SVG 字面量共 178.8 KB；
  vendor 侧是 react-query / lucide / react-i18next / radix 这些真底座，砍不动。
- 顺手把账本里「顺带固化 needsLightBacking 名单」那条也量了再下结论：
  154 个 key 全查一遍的热路径 **0.041 ms**，冷路径（102 个 SVG 解析取色）一次性
  1~5 ms。固化名单只能省掉更小的那项，还要用一个 Set 换一个 Map——不做。
- 状态：**已诊断，不改**（P2-2 保留为待办，改动面大、收益薄）

#### 试过并撤销：给规范化输出缓冲按原始长度预分配

- 想法：`canonicalize_json_string_if_parseable` 的 `canonical_json_string` 用
  `String::new()` 起步，倍增到几十 KB 要十几次 realloc、每次把已写内容整段拷一遍。
  而调用方手里就有原始文本——它的长度正是规范化输出的上界（键排序只改顺序、
  不改字节数），按它 `with_capacity` 就能省掉这段拷贝。
- 实测（debug、同进程同体、2000 轮、两条路径输出逐字节一致）：
  text=27 648 B → out=27 648 B，零容量 2502.06 µs、预分配 2449.56 µs，**-2.1%**。
- 结论：**撤销**。这段的成本几乎全在逐字符串转义与键排序上，倍增拷贝只占 0.2% 量级，
  为它加一个「带容量的第二入口」不划算。账本留数，下轮别再试。

#### 试过并回滚：用 manualChunks 把预设目录单独切块

- 想法：预设目录是纯数据，`manualChunks` 把它切成独立块，授权中心这类只用界面逻辑的
  懒加载块就不会被它拖着。
- 实测（`pnpm build:renderer`）：

  | 指标 | 切之前 | 切之后 |
  | --- | --- | --- |
  | `main-*.js`（入口） | 643.22 kB | 599.84 kB |
  | `AuthSettingsPanel-*.js` | 912.47 kB | 313.83 kB |
  | 全量 JS 产物 | 6 761.8 kB | 6 763.4 kB |
- 入口确实小了 43.38 kB、总量没涨，但**每一个带 JSX 的懒加载块都变成了「静态
  import provider-presets」**。根因不是预设本身，而是 rollup 把
  `react/jsx-runtime`（预设文件和界面共用）塞进了手动块：904 字节的
  `alert-*.js` 也得 `import{r as a,j as s}from"./provider-presets-..."`。
  另给 `src/lib/utils.ts`（`cn`）单独切一块也只解决一半——`cva` 那条路径同样中招。
- 决定性证据：打开「授权中心」这一个界面，静态依赖总字节

  | 懒加载界面的静态依赖（不含已在内存的入口块） | 自动分块 | manualChunks 切预设 |
  | --- | --- | --- |
  | 授权中心（`AuthCenterPanel`） | 596.42 kB | 1 294.6 kB |

  多出来的约 700 kB 就是那份不显示的预设目录，「授权中心不下载预设」的初衷
  恰好被反过来。给共享助手单独切块也堵不住，jsx-runtime 这类跨整个界面的运行时
  依赖总会落进手动块。

  ⚠️ 这两栏都是「静态 import 闭包的字节和」。第一次测量时 dist 里有上次实验的
  残留文件（`emptyOutDir` 不清 root 之外的目录），导致自动分块那栏一度被记成
  1 230.7 kB；清空重建后修正为 596.42 kB。manualChunks 那栏是同一次污染构建里
  测的，只当量级看，方向和结论不变。
- 处置：**回滚**，配置回到原始的自动分块。结论：这块的分块边界得由「界面自己按需
  拉预设」来解决（表单里改成动态 import），不是 manualChunks 能一步到位的。
- 顺带记下一个诊断事实：`alert-*.js` 这种小块的 import 语句是判断「某个大块被谁
  拖着」的最快线索，`__vite__mapDeps` 里的名字只是懒加载依赖表、不代表启动会拉。

- `src-tauri/src/proxy/forwarder.rs` 10 134 行，`forward()` 单函数约 1 700 行；
  `handlers.rs` 4 988 行。编译单元和 I-cache 压力大，LLVM 的跨函数优化空间受限。
- 配置模块族：claude / codex / dsh / hermes / kimi / mcode / omp / openclaw / opencode /
  pi / agy / copilot / gemini / grok / claude_desktop 十几份近同构读写处理器
  （codex_config.rs 一份 268 KB）。
- 前端懒加载块仍偏大：`AuthSettingsPanel-*.js` 846.60 kB；
  `pnpm build:renderer` 有 6 个块 > 500 kB（main 655 kB、disableContextMenu 580 kB、
  index 444 kB、UsageTrendChart 360 kB…）。
- 入口块仍静态拖着 `src/icons/extracted/index.ts`（187 KB 源码，约 126 个图标文件）：
  大 PNG 由 Vite 作为独立资源发出，JS 侧开销是 URL 表，暂无收益，先记为未决。

#### 评估后不改：`providerConfigUtils.ts` 按「通用 / Codex TOML」拆开

- 动机：它在入口静态图里（`ProviderList` 引 `extractProviderBaseUrl`、`ProviderCard`
  引一组函数），而 1785 行里有约 1500 行是表单专用的 Codex TOML 读写。
- 卡点：**交叉依赖**。要留下来的 `extractProviderBaseUrl` 有一个分支要调
  `extractCodexBaseUrl`（Codex 组的导出），要么把它也搬去 Codex 组（那
  `ProviderList` 仍拖着整份 Codex 文件，收益归零），要么让通用模块反向依赖
  Codex 组（循环依赖）。
- 结论：不改。真正要做就得先解掉这个交叉（例如 `extractProviderBaseUrl` 的 TOML
  分支改成调用方注入），那是另一个任务。

### 试过并拆除：给 `forward()` 分段插桩找热点

- 想法：`full forward, pure passthrough` 约 0.9 ms，而 `body_stage_costs` 的各段
  （深拷贝 0.091 / 私有扫描 0.001 / 序列化 0.385 / 解析 0.145 ms）加起来对不上，
  想知道剩下的大半花在哪。于是在 `forward()` 里按「决策与 Cow 变换 / 构造有序头 /
  序列化与 send / 响应整流」插了累计计时器。
- 拿到两个可靠结论（其余数字被下面两个坑污染，不采信）：
  - **构造有序头的循环不是热点**。把它按同样的比较顺序与 append 复刻成独立微基准
    （`header_loop_micro`）：402 轮共 264 µs、**每轮 0.657 µs**。插桩一度显示它
    占 79%，是计时错位造成的假象。
  - **路由决策 + Cow + Copilot 优化器 + transform 决策整段只占 1.3%**（256 µs/次）。
- 处置：**插桩全部拆除**，`forwarder.rs` 回到已提交状态（`cargo test --lib`
  5007 passed 复核过）。没有产出可落地的改动，但下一轮不用再怀疑这两个地方。
- 两个把这次测量毁掉的坑，记账防复发：
  1. **`Instant` 在 async fn 里被同名 shadow 会让分段计时张冠李戴**：`let __t =
     Instant::now()` 在循环前定义一次、循环后又 `let __t = Instant::now()`，
     后面结算时用的是后者，第一段就被算成了整个循环 + 之后的一切。
  2. **PowerShell 的 `Set-Content -Value <string[]>` 会把整个数组 join 成一行**，
     文件直接报废。往 Rust 源码里插多行只能用 `apply_patch`，或
     `Set-Content -Value ($arr -join "`r`n")`。
- 另外确认：dev profile 即使 `opt-level = 2` 与 release 仍差一个量级，
  「跨轮单测只当量级、同体对照才可信」这条对 debug 同样成立。

- 上一版账本用 `1 230.7 kB` 当自动分块的基线，那是**被污染的数字**：
  Vite 的 `build.emptyOutDir` 不清 root 之外的 outDir（这里 root=`src`、outDir=`../dist`），
  上次实验的 chunk 文件会残留，前后比的是两份不同源码状态。清空 dist 重建后，
  自动分块打开授权中心的静态依赖是 **596.42 kB**。账本已订正。
- 方法论：凡是「改动前 / 改动后」比字节，先手工清空构建目录。

| # | 项 | 关键改动 | 验证 |
| --- | --- | --- | --- |
| 1 | P0-1 | `write_json_string` 就地转义 + `SmallVec` 键表 | 5005 + 9 个规范化测试全绿；分配计数随字符串数脱钩 |
| 2 | P0-2 | 拆出 `src/config/copilotEnv.ts` | 入口块 -88.11 kB；typecheck + 4 个预设测试 |
| 3 | P0-3 | 合并/删除 SSE 扫描同构实现 | sse 测试不变，全量测试通过 |
| 4 | P0-4 | xAI 整流透传分支一次分配 | 该文件测试不变 |

#### 审计后不改：聚焦重拉与 IPC 批量查询

- 主 `queryClient` 用默认 `staleTime: 0` + `refetchOnWindowFocus: true`，即每次窗口
  聚焦会把 stale 的 query 全量重拉一遍。看着像浪费，但取证后判定是**有意设计**：
  - `src/tray-panel/main.tsx:17` 显式写了 `refetchOnWindowFocus: false`——作者知道
    这个开关，并只在托盘面板关掉它；
  - 20+ 处 query 按重要性分别配了 `staleTime`（30 s / 60 s / 5 min / `Infinity`），
    其中 `useSkills`、`useHermes`、`useOpenClaw`、`useSidebarStatus` 这类常驻查询
    都给了值；剩 54 个 `useQuery` 没写 `staleTime`，多数在懒加载组件里
    （`PiNativePromptResources` / `FetchedModelPicker` / `UsageDashboard`…），
    只在挂载期间才会被聚焦触发。
  - 「回到 app 就看到外部 CLI 改过的配置」正是聚焦重拉在这里的价值，mutation 又都
    主动 `invalidateQueries`，加 `staleTime` 不改善未知路径、只削弱这条已知路径。
- 结论：不改。真要动就逐 query 过语义，且收益无法在本仓库里量化。
- 顺手确认两处此前没细看的每请求路径都已经优化到位，下一轮不用再查：
  - `unsupported_param_rectifier::cached_params`：两层用 `&str` 直查
    （`String: Borrow<str>`），不命中路径零分配，命中才 clone + sort；
  - `commands/provider.rs::get_providers`：`spawn_blocking` + `ProviderService::list`，
    返回的 `IndexMap<String, Provider>` 是卡片渲染的固有数据面，要瘦身得改 API 形状。


### 测量口径订正

### P2-3 测试覆盖缺口：33.7% 的源文件没有任何测试引用

- 扫法（无需新依赖，`@vitest/coverage-v8` 没装、装它要改锁文件）：
  把 `tests/**` 与 `src/**/*.test.*` 共 339 个测试文件拼成一份文本，
  对 `src/**` 下 489 个非测试源文件逐一找它的引用路径
  （`@/…` 全路径 + `./name` / `../name` 相对路径，四种都试）。
- 结果：**165 / 489 = 33.7% 无人引用**。最大几个：

  | 行数 | 文件 |
  | --- | --- |
  | 830 | `src/components/sessions/reader/SessionReaderHeader.tsx` |
  | 736 | `src/components/settings/auth/ManagedAccountsGroup.tsx` |
  | 583 | `src/components/prompts/PiPromptPanel.tsx` |
  | 529 | `src/components/proxy/AutoFailoverConfigPanel.tsx` |
  | 511 | `src/components/settings/sections/RoutingSection.tsx` |
  | 341 | `src/components/providers/forms/CodexConfigSections.tsx` |

- 这只是上界：这些多数是被父组件渲染、由 `App.test.tsx` 这类集成测试间接碰到的。
  筛法是「找没有测试直接 import 的」，漏不掉间接覆盖，所以不能当真实覆盖率用。
- 状态：**待办**（按「逻辑重 + 纯函数」排序逐个补；下面第一个已经补完）

#### P2-3-1 `useTemplateValues` 的四条核心路径此前零直接覆盖

#### P2-3-2 图标取色逻辑四条纯函数零覆盖，另发现一处注释与代码不一致

- 位置：[`src/icons/extracted/index.ts`](src/icons/extracted/index.ts)
- 现状：`hexChannels` / `paintedColors` / `isDarkInk` / `adaptSvgToTheme` 四条
  此前只有 `needsLightBacking` 被间接跑到，剩下全是零覆盖。而它们是这个文件里
  唯一有分支的地方：三位/四位 hex 缩写展开、`<defs>`/`<clipPath>`/`<mask>`
  遮罩色不算墨色、单墨色图标才改写成 `currentColor`、遮罩块在改写时必须原样保留。
- 处理：导出 `hexChannels` 与 `paintedColors`（另两个本来就是 exported），
  新增 [tests/icons/extractedColorLogic.test.ts](tests/icons/extractedColorLogic.test.ts)
  21 个用例，把上面每条分支钉住。
- 状态：**已完成**
- 测试：`pnpm vitest run tests/icons/extractedColorLogic.test.ts` 21 passed；
  `pnpm typecheck`、prettier 通过；全量 `pnpm vitest run` **4 075 passed / 0 failed**
- ⚠️ **发现一处注释与代码不一致（未改，只记录）**：
  `isDarkInk` 的注释写「三个通道都很低（深红、深蓝这类有色的深色不算）」，
  但实现是 `Math.max(...rgb) <= 0x50`——于是 `#300000`（深红）、`#000030`（深蓝）
  都判成墨色，与注释相反。影响面：这类图标会被改写成 `currentColor`，
  在深色卡片上跟着文字色变浅。测试按**现实行为**断言并把分歧写进用例名，
  没有改代码——要改属于视觉决策，得先看图。

- 位置：[`src/components/providers/forms/hooks/useTemplateValues.ts`](src/components/providers/forms/hooks/useTemplateValues.ts)
- 现状：`collectTemplatePaths` / `getValueAtPath` / `setValueAtPath` /
  `applyTemplateValuesToConfigString` 四条**模块私有**，只能隔着 hook 间接覆盖，
  而 hook 的情形又大多走不到那些分支。`applyTemplateValuesToConfigString` 一家就有
  六个分支（无模板键 / 当前配置为空、数组、对象、其他 / 无占位符 / 顶层替换 /
  解析失败兜底），一条都没被直接跑过。
- 处理：把这四个函数与两个路径类型导出（注释写明「导出只为单测」），
  新增 [tests/hooks/useTemplateValues.test.tsx](tests/hooks/useTemplateValues.test.tsx)
  21 个用例，每个分支至少一条：占位符收集（含 `$key` / `${key` 这种不完整写法、
  根节点自身是占位符、数组下标）、取值的 null/undefined 短路、
  setValue 沿途补建对象/数组与类型不匹配时重置容器、
  以及应用时的「只改占位字段、保留用户其他字段」「数组当前配置不被冻住」
  「空配置当空对象」「非 JSON 兜底回填预配置」。
- 状态：**已完成**
- 测试：`pnpm vitest run tests/hooks/useTemplateValues.test.tsx` 21 passed；
  `pnpm typecheck` 通过；全量 `pnpm vitest run` 4 053 passed / 1 failed
  （失败的是改动前就存在的 `App.test.tsx` 执行时长 flake，单独跑 3.7s 通过）
- 踩坑：夹具里 `editorValue` 必须填上。`applyTemplateValues` 优先读
  `editorValue`、只在它为 `undefined` 时才回落 `defaultValue`，而 hook 初始化时
  本来就把它写成 `editorValue = defaultValue`——夹具照真实形状给才对，
  否则六条断言会全挂在「模板值没生效」上。

#### P2-3-3 把剩余 165 个未覆盖文件按「有没有可测的纯逻辑」分类收口

- 对 165 个未覆盖文件再扫一遍「非导出的顶层函数/常量」数量，**27 个文件有 ≥3 个**，
  但其中一大半是 UI 基元（`ui/dropdown-menu` 15 个、`ui/table` 8 个、`ui/card`
  6 个、`ui/command` 6 个、`ui/tooltip` / `tabs` / `popover` 各 4 个）——那些私有
  「函数」是 forwardRef 包装和 variant 工厂，不是业务逻辑，测它们等于测设计系统。
  真正该补的集中在 6 个文件。
- 已按此标准补掉第二个：[`src/tray-panel/usePanelTheme.ts`](src/tray-panel/usePanelTheme.ts)
  66 行、4 个私有函数，新增 [tests/hooks/usePanelTheme.test.ts](tests/hooks/usePanelTheme.test.ts)
  9 个用例。钉住的是它的三态语义：`explicit` 为 `null` 表示跟随系统、
  `true`/`false` 表示用户显式选了 dark/light；以及 localStorage 抛异常或存了
  垃圾值（如 `"solarized"`）时都回落系统、另一个窗口改选择后 `refreshKey` 变化要跟着变。
- 状态：**已完成**
- 测试：`pnpm vitest run tests/hooks/usePanelTheme.test.ts` 9 passed；
  `pnpm typecheck`、prettier 通过；全量 `pnpm vitest run` **4 084 passed / 0 failed**
- 剩余待补（都是渲染型组件，逻辑散在 JSX 里，逐个性价比低）：
  `sessions/reader/SessionDiff.tsx`（`buildInputDiff` 是真 diff 逻辑，值得单独抽出来测）、
  `sessions/reader/SessionStepDetails.tsx`、`sessions/SessionItem.tsx`、
  `settings/sections/RoutingSection.tsx`、`usage/UsageSettingsSheet.tsx`。
- 顺带纠正上一条记录里的一个误判：之前把 `useTemplateValues` 的 renderHook OOM
  归因为「测试结构」。这一轮用 `renderHook` 测 `usePanelTheme` 一次通过，
  说明 renderHook 本身没问题——那个 OOM 的真实原因是我的 props 工厂每次渲染
  新建 `presetEntries`，而 hook 的 `useMemo → useEffect → setState` 链条放大了它。
  记账改为上述说法，并给 `usePanelTheme` 的测试加了 `documentElement` class 清理，
  避免深色类污染同文件后续用例。

#### P2-3-4 `parseDiff` 十二条分支零覆盖，另记两处非直观行为

- 位置：[`src/components/sessions/reader/SessionDiff.tsx`](src/components/sessions/reader/SessionDiff.tsx)
  `parseDiff`（88 行，`SessionDiff` 组件唯一的真实逻辑）
- 现状：unified diff（git diff / Codex FileChange）与 apply_patch 两种文本的解析
  全是私有函数，只能隔着组件触达。
- 处理：导出 `parseDiff`，新增
  [tests/components/sessions/reader/parseDiff.test.ts](tests/components/sessions/reader/parseDiff.test.ts)
  22 个用例，覆盖：文件头成对判定（孤立 `--- ` 不算文件头）、`index` 行只在首个
  hunk 之前忽略（hunk 里以 `index ` 开头的 prose 要留下）、`/dev/null` 目标丢弃、
  多 hunk 之间插 gap 并重置行号、hunk 头带 `,count` 与尾部散文、hunk 头解析失败时
  行号不赋值也不递增、CRLF、`diff --git` / `*** Begin Patch` / `\ No newline`
  等元数据行跳过、apply_patch 的 `Add/Update/Delete File` 与 `*** Move to:`、
  连续同路径文件头合并、中间空行保留而尾部空上下文行修剪。
- 状态：**已完成**
- 测试：`pnpm vitest run tests/components/sessions/reader/parseDiff.test.ts`
  22 passed（初版 4 条断言失败，全是我预期写错，已逐个核对真实行为后修正，代码未动）；
  `pnpm typecheck`、prettier 通过；全量 `pnpm vitest run` **4 106 passed / 0 failed**
- 顺带记下两处非直观行为（测试按现实钉住，未改代码）：
  1. 孤立的 `--- x`（下一条不是 `+++ `）会落进「`-` 开头」的 del 分支，
     于是内容是 `-- x`（剥掉一个 `-`），不是 context 行。
  2. 整行空白不算「空上下文行」：剥掉行首一个空格后还剩两个空格，而末尾修剪只剥
     `text === ""` 的那些，所以 `parseDiff("   ")` 会留下一条 context。

#### P2-3-5 `RoutingSection` 的四个校验/格式化函数零覆盖

- 位置：[`src/components/settings/sections/RoutingSection.tsx`](src/components/settings/sections/RoutingSection.tsx)
  `isValidListenAddress` / `isValidClientHost` / `defaultClientHost` / `formatUptime`
- 现状：路由设置页的输入校验与展示格式化全是私有函数，只能隔着设置页点出来。
  其中「监听地址合法但客户端地址不合法」这个分歧（`0.0.0.0` / `::` 两侧结论相反）
  是最容易点错的一处。
- 处理：导出四个函数（注释写明「导出只为单测」），新增
  [tests/components/settings/routingValidation.test.ts](tests/components/settings/routingValidation.test.ts)
  23 个用例：IPv4 八位组上限（`256.1.1.1` / `999.999.999.999` 否掉）、四段必须是
  完整四段、IPv6 必须能过 `[..]` 包装后的 URL 解析（`::gg` / `12345::` 否掉）、
  主机名标签规则（不能以 `-` 开头或结尾、不能连续点、不能有下划线/空格）、
  253 总长上限 vs 63 单标签上限的区别、uptime 一小时以上走「时+分」、
  以下只报分且负数夹到 0。
- 状态：**已完成**
- 测试：`pnpm vitest run tests/components/settings/routingValidation.test.ts`
  23 passed（初版 1 条断言失败，是我把「253 个字符的单标签」当成了触发总长上限——
  实际它是被 63 字符单标签规则否掉的；改成 4×63+3 点 = 255 才真正触发前瞻，
  代码未动）；`pnpm typecheck` 通过；全量 `pnpm vitest run` 4 128 passed / 1 failed
  （失败的是已知的 `App.test.tsx` 执行时长 flake，单独跑 3.7s 通过）
- ⚠️ **又发现一处注释与代码不一致（未改，只记录）**：
  `isValidClientHost` 的注释说「`0.0.0.0` / `::` 客户端连不上」，
  但 `::1` 是**合法**的客户端地址——它既不是 `::` 也不是 `0.0.0.0`，
  会走 IPv6 字面量分支被判 true。测试按现实行为钉住（`isValidClientHost("::1")` 为
  true），因为这是用户可感知的正确行为（`::1` 确实连得上本机）。

#### P2-3-6 会话行高与附加摘录的对应关系零覆盖

- 位置：[`src/components/sessions/SessionItem.tsx`](src/components/sessions/SessionItem.tsx)
  `extraSnippetsOf`（私有）/ `getSessionRowHeight`（已导出但没人测）
- 现状：虚拟列表按 `getSessionRowHeight` 排版，而行内实际渲染几条附加摘录由
  `extraSnippetsOf` 决定。两者只要差一条，整段滚动就会漂——而这个对应关系
  此前一条测试都没有。
- 处理：导出 `extraSnippetsOf`，新增
  [tests/components/sessions/sessionRowHeight.test.ts](tests/components/sessions/sessionRowHeight.test.ts)
  8 个用例，钉住：`slice(1)` 丢掉第一条（它已显示在行内）、无命中 / 只有一条 /
  选择模式下都不列摘录、基础高度 56、每条附加摘录 +20 且整块 +6 内边距、
  选择模式下高度收回基础值（与摘录为空保持一致）、以及「extras 计数与高度算术」
  在 0/1/2/3/5/8 条上逐一对齐。
- 状态：**已完成**
- 测试：`pnpm vitest run tests/components/sessions/sessionRowHeight.test.ts`
  8 passed；`pnpm typecheck`、prettier 通过；全量 `pnpm vitest run`
  **4 137 passed / 0 failed**
- 踩坑两处（都不改代码）：夹具要补 `messageIndex` 字段（`SessionContentSnippet`
  的必填项，少一个就过不了 tsc）；`highlights` 用 `never[]` 会被推断成
  `never[]` 而不是 `SessionTextRange[]`，补字段后自然消失。

#### 覆盖缺口扫描的最终状态

截至本轮，账本 P2-3 系列已按「逻辑重 + 纯函数」标准补掉 6 个文件、
共 **96 个用例**（useTemplateValues 21 / 图标取色 21 / usePanelTheme 9 /
parseDiff 22 / routingValidation 23 / sessionRowHeight 8）。剩余未覆盖文件
的私有函数已全部核查过一遍：`SessionStepDetails.tsx` 的 `prettyJson` /
`fullCommand` 是单行格式化包装，`UsageSettingsSheet.tsx` 没有私有顶层函数，
其余是 UI 基元——按同一标准继续补的边际收益已经很低。

### 第四轮：`log_request` 每请求重建常量 SQL，并把纯计算留在全局 DB 锁内

- 位置：[`src-tauri/src/proxy/usage/logger.rs`](src-tauri/src/proxy/usage/logger.rs)
  `UsageLogger::log_request`（每个代理请求都要在这里落一行 `proxy_request_logs`）
- 现状：原实现有三处叠加。
  1. `format!("{insert_verb} INTO proxy_request_logs (26 列) VALUES (?1..?26)")`：
     字符串内容只随动词在 `INSERT OR IGNORE` / `INSERT OR REPLACE` 之间变化，
     却每个请求都 `format!` 出一条约 1.4 KB 的 String（一次堆分配 + 一次整段拷贝）。
  2. `conn.execute(&sql, ...)`：rusqlite 的 `Connection::execute` 内部就是
     `prepare(sql)`，**每次调用都重新解析这段 SQL**。同类的 DAO 全都用了
     `prepare_cached`——`database/dao/providers.rs` 11 处、`dao/proxy.rs` 6 处、
     `dao/settings.rs` 2 处、`services/session_usage.rs` 4 处、`usage_stats.rs` 4 处；
     只有这条每请求的热路径没用。`services/usage_stats.rs:806` 的注释甚至已经写明
     「prepare_cached 稳定命中，也省掉每行的 format! 分配」——同一个优化在别处做了，
     唯独漏了这里。
  3. `lock_conn!(self.db.conn)` 在函数第 2 行就取锁，随后锁内做了 5 次 `to_string()`、
     `chrono::Utc::now()`、`is_cache_inclusive_app` 字符串比较、
     `UsageSemantic::from_log`（4 次 String clone）、`bound_error_message`。
     `Database::conn` 是 `Mutex<Connection>` 且**全进程唯一**（`database/mod.rs:82`），
     锁内多待一微秒就是所有 DAO 一起多等。

- 处理：列清单与 26 个占位符提成一份 `const`，两条语句按动词用 `LazyLock<String>`
  各拼一次（进程内首条日志时），之后每次只是一个 `&str` 选择；`execute` 换成
  `prepare_cached`；纯计算全部挪到取锁之前，`drop(stmt)` / `drop(conn)` 在写完行、
  发通知之前显式释放。锁的获取次数、持锁期间的库操作顺序（读 `load_existing_semantic`
  → 可能的兜底读 → INSERT）完全不变，原子性不变。
- 证据（`cargo test --lib --release perf_probe -- --ignored --nocapture`，
  同一进程内 A/B，两侧 SQL 文本由测试断言逐字节一致）：

  | 环节 | 改造前 | 改造后 | 降幅 |
  | --- | --- | --- | --- |
  | 拼 SQL | 0.160 µs/次 | 0.000 µs/次 | -100% |
  | 准备语句 | 7.609 µs/次 | 0.307 µs/次 | **-96.0%** |
  | 合计（每行日志） | 7.769 µs | 0.307 µs | **-7.46 µs** |

  同一轮探针的其它读数作参照：`full forward, pure passthrough` 1.097 ms、
  `serialize to bytes` 0.427 ms。这 7.46 µs 约占纯透传整条转发的 0.7%——绝对量不大，
  但它是每请求必付、且原先付在全局 DB 锁内的一份；并发落库时收益比串行读数更明显。

- 同类余量扫描：`\.execute\(&?\s*format!` 全仓扫完，生产代码只剩 2 处，都在
  `database/schema.rs` 的建表 / 迁移路径。另外 7 个出现 `let sql = format!` 的文件
  （`session_manager/content.rs`、`services/session_usage{,_opencode}.rs`、
  `services/usage_stats.rs`、`services/sql_helpers.rs`、`dao/mcp.rs`）插进来的都是
  表名 / 列名 / 可变 WHERE 子句，是**真动态**、提不成常量，且都不在每请求路径上。
  结论：**这一类只有 `log_request` 一处，已收口。**
- 状态：**已完成**
- 测试（新增 2 个，`proxy::usage::logger::tests` 从 11 → 13 个）：
  - `log_request_binds_every_declared_column`：写满一行、把 26 列逐一读回来比对，
    把「列数 / 占位符数 / 参数个数」钉在运行时。列清单是从 `format!` 提成常量的，
    差一个占位符只有真写一行时才报错，必须有能看见它的用例。
  - `both_log_sql_statements_declare_the_same_columns`：两条语句的列清单与 26 个
    占位符必须共用同一份，且各自 26 / 26 对齐。

### 第五轮：5 处配置写在全局 DB 锁内失效代理配置缓存

- 位置：`database/dao/proxy.rs`（`update_global_proxy_config` /
  `update_proxy_config_for_app` / `update_circuit_breaker_config` /
  `set_proxy_flags_sync`）、`database/dao/settings.rs`（`set_setting`）
- 现状：这 5 个写入函数都是 `let conn = lock_conn!(self.conn)` 开头，
  然后 `conn.execute(...)`，最后 `self.invalidate_proxy_request_config_cache()`。
  而 `Database::conn` 是**全进程唯一**的 `Mutex<Connection>`（`database/mod.rs:82`）、
  `proxy_request_config_cache` 是另有一把 `Mutex`——于是每一次配置写入都在持有
  `conn` 的情况下去拿第二把锁，把全局 DB 临界区无谓拉长。
- 取证（读侧确认无死锁、只是临界区白等）：`get_proxy_request_configs`
  （`dao/proxy.rs:236`）的读法是「先在 `{}` 块里取 `proxy_request_config_cache`
  并立刻释放，未命中再去 `spawn_blocking` 里取 `conn`」，**从不同时持有两把**，
  所以现在形不成环、没有死锁。但写侧这个顺序（`conn` → `cache`）与读侧相反，
  是一处随时会踩的锁顺序陷阱：读侧哪天改成持缓存锁查库，立刻死锁。
- 处理：5 处一律在失效缓存前 `drop(conn)`，先放全局 DB 锁再取缓存锁。
  写库与失效缓存的相对次序不变（仍是先写后失效），失败路径也不变
  （`?` 提前返回时同样不做失效）。
- 证据（`cargo clippy --lib -j 2 -- -W clippy::significant_drop_tightening`，
  同口径前后对比）：该 lint 命中从 **191 → 185**，`dao/proxy.rs` 18 → 14、
  `dao/settings.rs` 6 → 5，净清 6 处「临时值带显著 Drop 却拖到晚释放」。
- 状态：**已完成**
- 测试：无新增用例——这 5 个函数改的是锁的持有范围，不改任何可观测行为
  （写库结果、缓存失效时序、错误路径全同），现有 `dao/proxy`、`dao/settings`
  与 `services::proxy` 的用例即覆盖。`cargo test --lib` 5 009 passed /
  0 failed / 20 ignored、`cargo test --tests` 5 227 = 5 009 + 218 集成、
  clippy 0 warning、fmt 干净，与改前逐项一致。
- 余量：`significant_drop_tightening` 还剩 185 处。逐个看过，绝大多数
  （`dao/providers.rs` 26、`codex_oauth_auth.rs` 17、`usage_stats.rs` 12、
  `dao/skills.rs` 11…）是 guard 的最后一次使用就在函数末尾，收紧等于空转；
  少量像 `circuit_breaker.rs::transition_to_half_open` 那样能省三行原子操作的，
  收紧会改变「状态置位与计数器清零」的可见顺序、引入竞态，**判定不动**。

### 第六轮：`mcp/native.rs` 补上写入侧的对称抽取，四家同步逻辑不再各写一遍

- 位置：[`src-tauri/src/mcp/native.rs`](src-tauri/src/mcp/native.rs)（新增
  `write_mcp_servers_document` / `edit_mcp_servers_document`）、
  [`kimi.rs`](src-tauri/src/mcp/kimi.rs)、[`mcode.rs`](src-tauri/src/mcp/mcode.rs)、
  [`agy.rs`](src-tauri/src/mcp/agy.rs)、[`pi.rs`](src-tauri/src/mcp/pi.rs)
- 动机：`native.rs` 的模块文档自己写着「Pi / Kimi / MCode / AGy 四家的原生配置文件
  形状相同……读取校验与导入逻辑因此完全同构。差异只由 `NativeSource` 表达」——
  读/导入侧确实抽成了结构体，**写入侧却一直是四份 `sync_file` 各写一遍**。
- 取证（规范化行比对，只数 `#[cfg(test)]` 之前的生产代码）：56 行生产代码在
  kimi / mcode / agy / pi 中至少三个文件里逐行相同；占各自生产段的比例是
  kimi 72%、mcode 84%、agy 63%、pi 42%。四份复制的是同一套四步：
  `read_native` → 缺 `mcpServers` 就补空对象 → `as_object_mut().unwrap()` 取 servers
  → `to_string_pretty` + `atomic_write_private` 落盘。
- 处理：`edit_mcp_servers_document(path, label, tolerate_empty, edit)` 收掉这四步，
  `edit` 闭包只表达各家自己的合并策略，返回 `bool` 表示是否需要落盘——
  Pi 的「停用/删除一个不存在的条目就什么都不做」正好用 `false` 表达。
  AGy 原有「停用不存在的条目也照样重写文件」的行为**原样保留**（返回 `true`），
  不借机改语义。四家的 `sync_file` 因此各少一层缩进。
  顺带删掉 kimi / mcode / agy 三个只剩测试在用的 `fn read`（死生产代码），
  测试改调 `crate::mcp::native::read_native`；pi 的 `read` 仍被
  `remove_disabled_if_managed` 使用，保留。
- 证据（生产段行数，`git show HEAD:<path>` 与工作区对比）：

  | 文件 | 改前 | 改后 | |
  | --- | --- | --- | --- |
  | `mcp/kimi.rs` | 108 | 95 | -13 |
  | `mcp/mcode.rs` | 90 | 77 | -13 |
  | `mcp/agy.rs` | 129 | 115 | -14 |
  | `mcp/pi.rs` | 329 | 320 | -9 |
  | `mcp/native.rs` | 159 | 196 | +37 |
  | 合计 | 815 | 803 | **-12** |

  **行数上几乎是平的（-12）**，这一轮买的不是行数，是三件事：
  1. 四份 `document["mcpServers"].as_object_mut().unwrap()` 这个潜在 panic 点
     收敛成一处带不变式说明的 `expect`（`read_native` 已保证缺失或是对象）；
  2. 「确保 `mcpServers` 是对象 + 怎么落盘」从此只有一处可改，四家同步生效；
  3. `native.rs` 读/导入侧的 `NativeSource` 与写入侧的
     `edit_mcp_servers_document` 终于对称，和模块文档的说法一致。
- 状态：**已完成**
- 测试：`cargo test --lib mcp::` **123 passed / 0 failed**；无新增用例——
  这轮改的是代码组织，四家 `sync_file` 的可观测行为逐条未变（写库结果、
  失败路径、是否需要落盘全同），现有 kimi 2 / mcode 2 / agy 5 / pi 4 加
  `native` 的用例即覆盖。全量 `cargo test --lib` 5 009 passed / 0 failed /
  20 ignored、`cargo test --tests` 5 227 = 5 009 + 218 集成、
  `cargo clippy --lib --all-targets` 0 warning、`cargo fmt --check` 干净，
  与改前逐项一致。
- 审计后否定（同类扫描）：按固定行窗口做重复检测，全仓 379 个 `.rs` 扫完，
  只剩 109 个重复窗口，其中 `streaming.rs` / `streaming_codex_chat.rs` 那 22 个
  **全在 `#[cfg(test)]` 里**（同一个 `terminal_boundary_output` 测试抄了两家）、
  `provider_router.rs` / `tests_issue_7271.rs` 那 45 个是回归测试复刻生产代码。
  两边都不该抽——抽了会让两个 provider 的测试失去独立性。

### 第七轮：托盘面板不再为用量页签预加载 360 kB 图表块

- 位置：[`src/tray-panel/TrayPanel.tsx`](src/tray-panel/TrayPanel.tsx)（`UsageTab`
  由静态引入改为 `lazy`）
- 取证：先把两个入口的静态依赖图算出来（排除 `import type`，`@/` 别名从仓库根解析），
  再直接读 `dist/tray-panel.html` 数它到底预取了什么。结果：
  `tray-panel.html` 用 `<link rel="modulepreload">` 拉了 **4 个 JS 块**——
  `tray-panel-*.js` 25.19 kB、`disableContextMenu-*.js` 566.13 kB、
  **`UsageTrendChart-*.js` 351.92 kB**、`chevron-left-*.js` 0.14 kB，
  外加 102.44 kB 的 CSS，**初始载荷合计 1 045.8 KB**。
  链条是：`TrayPanel.tsx` 第 44 行静态 `import { UsageTab }` → `UsageTab` 静态引
  `recharts` + `@/components/usage/UsageTrendChart` → 那个 352 kB 的块
  （内部还拖着 babel / estree）。而 `UsageTab` 只在 `tab === "usage"` 时才挂载
  （第 1120 行在 `tab === "config" ? ... : (...)` 的 else 分支里），
  托盘默认停在 config 页签——**绝大多数悬停显示一次都不碰用量页签**。
- 处理：改成 `lazy(() => import("./UsageTab").then(m => ({ default: m.UsageTab })))`
  （跟 `App.tsx` 里 LazyAddProviderDialog 等命名导出的写法一致），
  渲染处包 `<Suspense>`，fallback 用面板里早已引入的 `Loader2` 转圈。
- 证据（`pnpm build:renderer` 后直接量 `dist/tray-panel.html` 引用的文件）：

  | 指标 | 改前 | 改后 | |
  | --- | --- | --- | --- |
  | 托盘初始载荷合计 | 1 045.8 KB | **612.6 KB** | **-433.2 KB / -41.4%** |
  | `tray-panel-*.js` | 25.19 KB | 19.99 KB | -5.20 KB |
  | 预加载的 JS 块数 | 4 | 3 | `UsageTrendChart` 不再预取 |
  | `UsageTrendChart-*.js` | 351.92 KB（预取） | 360.41 KB（按需） | 转为惰性 |
  | 主窗口初始载荷 | 1 294.4 KB | 1 294.3 KB | 不变 |

  主窗口那个共享块从 566.13 KB 变成 502.26 KB，但同时多出一个
  75.46 kB 的 `useMoneyFormat-*.js` 仍被 `index.html` 预取，两边抵消，
  **主窗口一个字节都没占到便宜**——这一轮是纯粹的托盘面板收益。
- 状态：**已完成**
- 测试：`tests/components/TrayPanel.test.tsx` 37 + `TrayPanelUsageTab.test.tsx` 3
  + `tests/tray-panel/menuAim.test.ts` 4 = **44 个全过**（`TrayPanelUsageTab`
  正是压用量页签挂载的）；`pnpm typecheck`、`pnpm prettier --check`、
  `pnpm vitest run` **4 137 passed / 0 failed**（336 个文件，连既有 flake
  这次也通过）、`pnpm build:renderer` 通过。
- 顺带记下的入口图审计（都判定不动）：
  - 主入口静态图 **211 个模块 / 2.29 MB**，托盘 **113 个 / 1.67 MB**；
    懒加载目标是 7 个 JSON 预设目录共 2.18 MB（切分正确）。
  - 静态图里有 **44 张图片共 961.8 KB**（`src/icons/extracted/*.png|jpg`）。
    查证后**不是 JS 体积问题**：Vite 的 `assetsInlineLimit` 默认 4 KB，
    超过的图片落成 `dist/assets/*.png` 独立文件，JS 里只有一个 URL 字符串，
    只在 `<img>` 真的渲染时才请求。小于 4 KB 被内联成 base64 的只有 7 张、
    约 12 KB，可忽略。
  - `src/icons/extracted/index.ts` 187.6 KB 里 **102 处内联 `<svg>` 字符串**
    才是真正的 JS 重量——账本 P2-2 已量过（0.2~0.4 ms 解析）并判定不改，沿用。
  - `deterministic_interaction_id(session_id)` 是 `session_id` 的纯函数，
    同一会话每请求重算一遍 SHA-256 + UUID。**判定不加缓存**：收益约
    0.5~1 µs/请求，低于自带 `Mutex<HashMap>` 加淘汰策略的开销，
    还会引入一个无界增长的风险点。
- TS 侧死导出复查（对应「不要写无效代码」）：扫 489 个非测试 TS/TSX 的
  **1 813 个顶层导出符号**，用「除定义处外全仓零引用」判定，
  **死导出 0 个**。账本早前「TS 侧没有死导出」的结论在当前代码上依然成立。

### 第八轮：切断桶文件 → mutations → sessions 这条边，会话阅读器代码离开主入口

- 位置：新增 [`src/lib/query/sessionKeys.ts`](src/lib/query/sessionKeys.ts)、
  [`sessions.ts`](src/lib/query/sessions.ts)、[`mutations.ts`](src/lib/query/mutations.ts)、
  [`SessionManagerPage.tsx`](src/components/sessions/SessionManagerPage.tsx)、
  `tests/lib/sessionsQuery.test.tsx`
- 取证：第七轮算入口静态图时发现 `components/sessions/reader/turns.ts`（33 KB）
  竟在主入口的静态图里。顺链查下去是一条三段边：
  `App.tsx` 第 33 行 `from "@/lib/query"`（**桶文件** `export *` 了
  queryClient / queries / **mutations** / proxy / subscription / pi 六个模块）
  → `mutations.ts` 第 19 行 `import { sessionKeys } from "@/lib/query/sessions"`
  → `sessions.ts` 第 14 行静态引 `buildTurnIndex, buildTurns` ← 来自
  `reader/turns.ts`。`mutations.ts` 只为拼两个 query key 就要 `sessionKeys`，
  却把整棵「会话阅读页数据层」拖进了主入口。
- **静态图只是线索，不是证据**：Rollup 会摇树，所以我直接到 `dist/assets/*.js`
  里搜能扛过压缩的字符串字面量。`turn_divider`、`slash_command`（`turns.ts`
  特有）**确实出现在 `main-*.js` 里**——不是误报。
- 处理：`sessionKeys` 与它依赖的 `stableKey` 搬到新的 `sessionKeys.ts`
  （`stableKey` 只在本文件内自用，`sessions.ts` 的 `ContentRef` 类型导入
  另有第 243 行在用、不动）。`mutations.ts` 与 `SessionManagerPage.tsx`
  改从新模块导入；`sessions.ts` 改为导入而非定义。没有留任何兼容再导出。
- 证据（`pnpm build:renderer`；字面量归属同样用搜索法核对）：

  | 指标 | 改前 | 改后 | |
  | --- | --- | --- | --- |
  | `main-*.js` 原始 | 643.31 kB | **616.46 kB** | **-26.85 kB** |
  | `main-*.js` gzip | 206.95 kB | **196.56 kB** | **-10.39 kB / -5.0%** |
  | 主窗口初始载荷 | 1 294.3 KB | **1 268.1 KB** | **-26.2 KB** |
  | `SessionManagerPage-*.js` | 172.19 kB | 193.55 kB | +21.4 kB（代码搬回它该在的懒加载块） |
  | `turn_divider` / `slash_command` 所在块 | `main-*.js` + 懒加载块 | **只剩懒加载块** | 已验证 |

  懒加载块变大是**对的**：那两段代码本来就只有会话阅读页用，搬过去才不浪费。
  净效果是主入口少解析 26.85 kB，懒加载页多 21.4 kB，总量还略降。
- 状态：**已完成**
- 测试：`tests/lib/sessionsQuery.test.tsx` + `SessionManagerPage.test.tsx`
  + `TrayPanel.test.tsx` + `TrayPanelUsageTab.test.tsx` = **86 个全过**；
  全量 `pnpm vitest run` **4 137 passed / 0 failed**（336 个文件）；
  `pnpm typecheck`、`pnpm prettier --check`、`pnpm build:renderer` 通过。
- 遗留观察（记录，未改）：`src/lib/query/index.ts` 这个桶仍在，
  `App.tsx` / 40 多处都从 `@/lib/query` 导入。它现在**没有**造成额外体积
  （改后搜索字面量确认阅读器代码已不在主入口），但只要有任何一个模块从
  `sessions.ts` 这类重模块导入轻量符号，就会重新把整棵树拖进入口。
- 又查了一处同类嫌疑，结论是**合法依赖、不动**：`components/settings/auth/
  ManagedAccountsGroup.tsx`（25 KB）也在 `main-*.js` 里（搜
  `authCenter.pending.reauth` / `authCenter.toast.reauthed` 两个特有字面量确认）。
  链条是 `ProviderList.tsx:57`（首屏组件）→ `DshAccount.tsx:7` →
  `ManagedAccountsGroup.tsx`。`ProviderList` 是为「DSH 登录了 DeepSeek 账号时
  在已添加最前面画一张官方账号卡」才用它的，是首屏的真实需要，不是桶文件那种误连。

### 第九轮：`ClaudeBoundAccountLine` 拆出去，首屏卡片不再拖 467 行的表单认证钩子

- 位置：新增 [`ClaudeBoundAccountLine.tsx`](src/components/providers/forms/ClaudeBoundAccountLine.tsx)、
  [`ClaudeOfficialSection.tsx`](src/components/providers/forms/ClaudeOfficialSection.tsx)、
  [`ProviderCard.tsx`](src/components/providers/ProviderCard.tsx)
- 取证：继续查主入口静态图里的可疑模块，链路是
  `App.tsx` → `ProviderList.tsx` → `ProviderCard.tsx:27` →
  `ClaudeOfficialSection.tsx` → `hooks/useManagedAuth.ts`。
  `ProviderCard` 只从那个 593 行的表单区块里拿一个小组件
  `ClaudeBoundAccountLine`（画「绑了哪个 Claude 账号」那行副标题），
  却把整棵表单区块拖进首屏——其中 `useManagedAuth.ts` 467 行 / 15.4 KB。
  用字面量搜索确认 `managedAuth.accountRemoved`、`codexOauth.loginExpired`
  这些 `useManagedAuth.ts` 独有的字符串**确实在 `main-*.js` 里**。
- 处理：`ClaudeBoundAccountLine` 连它自用的三个本地钩子
  （`useClaudeLoginSwitch` / `useClaudeLoginBlocked` / `useClaudeAccount`）
  一起搬进独立模块；`useClaudeLoginBlocked` 因为 `ClaudeOfficialSection`
  自己的账号选择也要用，从新模块导出、原文件改为导入，保持单一定义。
- 证据（`pnpm build:renderer`，字面量归属用搜索法核对）：

  | 指标 | 改前 | 改后 | |
  | --- | --- | --- | --- |
  | `main-*.js` 原始 | 616.46 kB | **581.02 kB** | **-35.44 kB** |
  | `main-*.js` gzip | 196.56 kB | **185.93 kB** | **-10.63 kB / -5.4%** |
  | 主窗口初始载荷 | 1 268.1 KB | **1 233.5 KB** | **-34.6 KB** |
  | `useManagedAuth` 独有字面量所在块 | `main-*.js` | 只剩 `ClaudeOfficialSection-*.js`（41.4 kB 懒加载块） | 已验证 |

  `ClaudeOfficialSection-*.js` 是这一轮新独立出来的懒加载块（42.35 kB），
  只在打开供应商表单时才加载。
- 状态：**已完成**
- 测试：`ProviderCard.claudeAccount.test.tsx` 5 + `ClaudeOfficialSection.test.tsx` 14
  + `ProviderList.test.tsx` 38 + `AddProviderDialog.test.tsx` 16 = **68 个全过**；
  全量 `pnpm vitest run` **4 137 passed / 0 failed**（336 个文件）；
  `pnpm typecheck`、`pnpm prettier --check`、`pnpm build:renderer` 通过。
- 踩坑（记下来，下次别重复）：**字面量必须选目标模块独有的**。我第一轮选
  `managed-auth-status` 判断 `useManagedAuth.ts` 在不在主入口，看着像命中，
  其实那个字符串同时是 `ClaudeOfficialSection.tsx` 里 `useClaudeAccount`
  的 queryKey——差点追错方向。改选 `managedAuth.accountRemoved`、
  `codexOauth.loginExpired` 这些只在该文件出现的才作准。
- 本轮会话累计（都本地验证后推送，未跑 Actions）：`main-*.js`
  643.31 kB → **581.02 kB（-62.29 kB）**，gzip 206.95 → **185.93 kB（-21.02 kB）**；
  托盘面板初始载荷 1 045.8 KB → **612.6 KB（-41.4%）**；
  `log_request` 每行省 7.46 µs；`mcp` 生产段 815 → 803 行。

### 下一轮候选（按证据排序，都未动手）

1. **拆掉 `src/lib/query/index.ts` 这个桶**：`App.tsx` 加 40 多处导入方直接写
   具体模块路径。当前它不造成额外体积（第八轮后已用字面量搜索确认），
   收益是**预防性**的——只要有模块再从 `sessions.ts` 这类重模块导轻量符号，
   就会重新把整棵树拖进入口。机械改动、无行为风险，但收益不可测量。
2. **`ManagedAccountsGroup`（25 KB）是否该留在主入口**：要先判断首屏那张 DSH
   账号卡是否用得上它的完整实现。若是，可把卡片要的那部分拆成更小的模块；
   若不是，链条本身就该断。**这是产品判断，等用户拍板。**
3. **P2-2 图标表**（`src/icons/extracted/index.ts` 187.6 KB / 102 处内联
   `<svg>` 共 178.8 KB）：仍在所有懒加载块的公共 chunk 里。账本已量过
   0.2~0.4 ms 解析、判定不改。要动就得整套方案（SVG 落独立文件 +
   `import.meta.glob(eager:false)` + `ProviderIcon` 异步化 + 核对
   currentColor 不退化），是独立排期的大件。

#### 评估后不改（第九轮补）：`providerConfigUtils.ts` 按「读 / 写」拆开

- 动机：主入口只用它的 7 个「读取 / 判断」类导出（`extractProviderBaseUrl`、
  `extractCodexBaseUrl`、`extractCodexWireApi`、`isCodexAnthropicWireApi`、
  `isCodexChatWireApi`、`extractCodexExperimentalBearerToken`、
  `hasExplicitNonOpenAiCodexModelProvider`），其余 18 个「写入 / 设置」类导出
  只被懒加载的供应商表单用。
- 闭合依赖算过了：主入口那 7 个的传递闭包是 9 个导出 / 740 行，
  闭包外 18 个导出 / 1 046 行，且**闭包内没有任何一条指向闭包外的反向边**——
  按读/写拆在依赖方向上是干净的，比早前「按通用 / Codex TOML 拆」那条路更可行。
- **卡点有两个，都比账本早前记的那条更硬**：
  1. **私有助手被读写两侧共用**：文件有 85 个顶层声明、57 个私有，
     `getTomlSectionRange` / `getTopLevelEndIndex` / `findTomlAssignments` /
     `tomlBasicString` / `unescapeTomlBasicString` 这些 TOML 扫描原语同时服务
     两侧（例：`getTomlSectionRange` 被 `extractCodexWireApi`【读】和
     `setCodexWireApi`【写】同时用）。要拆就得再抽第三块「TOML 原语」模块，
     1 786 行三重划分，改动面与出错面都很大。
  2. **Rollup 已经把写侧摇掉了**。用独有字面量验证：`setCodexModelName` 独有的
     `default_subagent_model` **只出现在 `AuthSettingsPanel-*.js`（懒加载块）**，
     不在 `main-*.js`。也就是说那 1 046 行本来就没进主入口，拆了也拿不到体积。
- 结论：**不改，且比早前那次评估更该不改**。早前卡在
  `extractProviderBaseUrl` → `extractCodexBaseUrl` 的交叉依赖，实际真要做是
  抽 TOML 原语模块；而抽之前先量一下就会发现摇树已经代劳了。
- 方法学收获：**「某个导出只在懒加载方用」不等于「它的代码在主入口里」**。
  先搜一个该导出独有的字面量、确认它真的在 `main-*.js`，再决定要不要拆。

### 第十轮：测试 HOME 隔离的锁域审计——否定一项改写计划，记录一处跨组隐患

- 起因：接手材料里有一条「27 个文件还在用私有测试 HOME 轮子，应迁移到
  `TestHomeScope`」的候选。先取证，结论是**那条统计口径错了**，不该照着动手。
- 取证（按 `src-tauri` 全量 `.rs` 分三层数，不混在一起看）：

  | 统计口径 | 数量 | 性质 |
  | --- | --- | --- |
  | 定义 `static LOCK: OnceLock<...>` 的文件 | 31 | **混入了两类无关东西**，不能直接当轮子数 |
  | ├ 其中是 `hermes_write_lock` / `opencode_config_lock` 这类 | — | **生产环境的配置文件写锁**，跟测试 HOME 无关 |
  | 直接 `set_var("CC_SWITCH_TEST_HOME", ...)` 的文件 | 34 | 真正在改测试主目录的 |
  | └ 其中同时调了共享 `test_home_lock()` 的 | **29** | 合规：走共享锁域 |
  | └ 没走共享锁、只靠 `#[serial]` 的 | ~~**4 + 1**~~ → **1 + 1** | `agy_smoke_tests.rs` + `tests/support/mod.rs`（另 3 个源文件已在 `8d8eaf17` 补齐） |

- **否定那条 27 文件迁移计划**：
  1. 31 个 `static LOCK` 里绝大多数是配置文件写锁（`hermes_write_lock`、
     `opencode_config_lock`…），迁移它们毫无意义；
  2. `tests/support/mod.rs` 的 `test_mutex()` **不是 `test_home_lock()` 的重复**：
     `test_home_lock()` 带 `#[cfg(test)]`，只存在于 lib 的单测二进制里，集成测试
     二进制根本看不到它；而 `support/mod.rs` 还多做三件它做不到的事——按
     `NEXTEST_TEST_GLOBAL_SLOT` 分槽位、覆盖 `LOCALAPPDATA`（Windows 上 Claude
     Desktop 的真实配置目录）、清掉 `PI_CODING_AGENT_DIR`。二者不可合并。
  3. 真正待处理的只剩 4 个源文件，不是 27 个（其中 3 个已在 `8d8eaf17` 补齐）。
- 修掉一处真实隐患（`8d8eaf17`，原先只记不修，因为当时手上没有证据）：
  `#[serial]`（serial_test 的全局锁）与 `test_home_lock()` 是**两把独立的锁**。
  实测 **28 个文件同时用两者**（锁序一致：harness 先拿 `#[serial]`，测试体内再拿
  home_lock，所以不会死锁），但另有 4 个文件只拿 `#[serial]` 就改 `CC_SWITCH_TEST_HOME`。
  于是「只用 `#[serial]` 的一组」与「走 home_lock 的一组」**可以并行运行，却都在改同一个
  全局环境变量**——正是 `config.rs:49` 注释警告的「并行跑时会互相换掉临时主目录」。
  **修法**：`app_config.rs` / `codex_config.rs` / `agy_config.rs` 三处测试 HOME guard
  在 set 与还原两侧都由 `test_home_lock().blocking_lock()` 持住；`agy_config.rs` 的
  `HomeGuard` 改成把锁放最后的 3 字段 tuple，使锁覆盖整个生命周期、并在所有字段 drop
  之后才释放。`agy_smoke_tests.rs` 按设计排除：它唯一用例是 `#[tokio::test]`，
  `blocking_lock()` 在 async 上下文里会 panic，且自带 `#[ignore]`（需
  `CCS_AGY_SMOKE_CONFIG` / `CCS_AGY_SMOKE_BIN`），常规 `cargo test` 根本不跑它。
  **本轮补上的证据**：改前改后 `cargo test --lib` 都是 5 009 passed / 0 failed /
  20 ignored，`cargo test --tests` 5 227，`clippy --lib --all-targets` 0 warning，
  `fmt --check` 干净——锁域统一没有引入回归，也没有死锁。竞态本身仍是概率性缺陷，
  绿灯不等于「已证明它被触发过」；本条的结论是**把它从可能降级为不可能**。
- 顺带订正一条旧数字：P1-1 里写的「全仓有 37 个文件会改它」，当前实测是
  **34 个**（`set_var("CC_SWITCH_TEST_HOME"` 的出现文件数）。P1-1 的结论
  （不给 `get_home_dir()` 加缓存）不受影响，仍然成立。
- 状态：**审计完成；27 文件迁移计划已否定；3 文件锁域统一已落地（`8d8eaf17`），`agy_smoke_tests.rs` 因 `#[ignore]` + async 上下文按设计排除**

### 验证矩阵（本账本全部改动的收口）

后续每个提交都在本地跑过下面这些，不依赖 CI：

| 维度 | 命令 | 结果 |
| --- | --- | --- |
| Rust lint | `cargo clippy --lib --all-targets` | 0 warning |
| Rust 格式 | `cargo fmt --check` | 干净 |
| Rust 单元 + 集成测试 | `cargo test --lib -j 2` | 5 009 passed / 0 failed / 20 ignored |
| Rust 集成测试 | `cargo test --tests -j 2` | 5 227 = 5 009 lib + 218 集成 |
| Rust release 编译 | `cargo test --lib --release perf_probe` | 28m30s 通过（编译 release profile 的 lib 并链接测试二进制，覆盖面大于单独 `cargo build --lib --release`） |
| Rust 全部 feature | `cargo clippy/test --all-features` | 0 warning / 5 007 passed |
| 前端类型 | `pnpm typecheck` | 通过 |
| 前端格式 | `pnpm prettier --check` | 通过（`useClaudeAccountQuota.test.tsx` 是改动前就有的既有问题，未动） |
| 前端测试 | `pnpm vitest run` | 4 137 passed / 0 failed（336 个文件） |
| 前端构建 | `pnpm build:renderer` | 通过（`main-*.js` 581.02 kB / gzip 185.93 kB；主窗口初始载荷 1233.5 KB；托盘面板初始载荷 612.6 KB） |

**release 编译是这一轮才补的**：此前所有 Rust 改动只在 debug/test profile 验过，
而打包走 release（fat LTO + `codegen-units = 1`），两者不是同一套代码生成路径。
`cargo build --lib --release` 实测 9m18s 通过。

**构建缓存要定期清**：`src-tauri/target` 一路涨到 42 GB（`debug` 39.4 GB，其中
`debug/incremental` 12.69 GB；`release` 2.61 GB），把 C 盘从 25 GB 空闲吃到 11 GB。
按 AGENTS.md 第 5 条清掉这两个可再生目录后回到 24.3 GB。`debug/incremental` 是
纯编译器缓存、`release` 是可再生产物，删了只让下一次构建变慢，不影响任何源数据。

- **C 盘剩余空间低于约 3 GB 时，`cargo test --tests` 会以 `link.exe` 退出码 1318
  随机失败**（2026-11-12 实测）：16 个集成测试二进制并行链接，先后在
  `provider_service` 和 `skill_sync` 两个不同目标上报同样的错，`-j 1` 也复现。
  当时 C 盘只剩 2.60 GB、`debug/incremental` 15.72 GB。逐文件删掉
-  `src-tauri/target/debug/incremental`（3 933 个文件、纯编译器缓存）后回到
  17.15 GB，同样命令立刻全过。**看到 1318 先看磁盘，不要怀疑自己的改动。**
- 另一条实测：`cargo test --lib --release perf_probe` 从零编 release 会把 C 盘
  从 17.15 GB 吃到 8.79 GB（约 8.4 GB），跑完紧接着 `cargo test --tests` 就直接
  `os error 112 磁盘空间不足`。所以顺序应该是：先清 `debug/incremental` 与
  `release`，再跑 release 探针，最后跑集成测试；或者跑完 release 立刻把
  `target/release` 删掉。删盘要盯住 `debug/incremental` 和 `release` 这两个
  可再生目录，`debug/deps` 不要碰。
  同轮再踩一次（11-12 晚）：清到 17.15 GB 后连跑 `cargo check --lib` +
  `cargo test --lib mcp::`，`debug/incremental` 重新长到 3.86 GB，
  接着 `cargo test --lib` 直接
  `rustc-LLVM ERROR: IO failure on output stream: no space on device`——
  **失败本身不留空间**，报错时空闲已经见底。再清一次 incremental（回收 3.78 GB）
  才全绿。所以每轮验证前都先 `Get-PSDrive C` 看一眼，别等报了错再清。
- 删除做法：`Get-ChildItem -Recurse -File` 之后逐个
  `[System.IO.File]::Delete($f.FullName)`，不用 `Remove-Item -Recurse`
  （AGENTS.md 的 Windows 安全规则），删完再滚掉空目录。删前用
  `Resolve-Path` 验一次目标确实在工作区内。

## 落地清单（第一轮，提交 da72f940）

## 踩坑记录

- **推送走 `jsnzkpg4.pages.dev` 加速地址会卡死**（2026-11-11 实测）：`git push` 挂住
  20 分钟后被 `OpenSSL SSL_read: Connection was reset` 打断；`git ls-remote` 同一地址
  要 649.7s。同批实测另两个加速地址：`seep.eu.org` 342.5s、
  **`down.nigx.cn` 14.3s**。所以推不动时直接换
  `git push https://down.nigx.cn/github.com/nizabushangtianne3/cc-switch.git main`
  （实测 3.8s 推完），推完补一句
  `git update-ref refs/remotes/origin/main <new-sha>` 把跟踪引用修正过来，
  不用改用户的 remote 配置。
补充（2026-11-12 复测）：三个地址的延迟会随时间波动——当天 `jsnzkpg4` 挂死、
`down.nigx.cn` 14.3s，次日 `down.nigx.cn` 一度 355s、`seep.eu.org` 342s，
再晚些 `down.nigx.cn` 回到 19s。所以别把这些数字当恒定值：推不动就按
「down.nigx.cn → seep.eu.org → jsnzkpg4」换，并给每次尝试记实际耗时。
再补充（2026-11-12 晚）：**「jsnzkpg4 会卡死」这条结论已被推翻**。同一天先后
试了三个地址推同一个提交：`down.nigx.cn` 返回 HTTP 429（限流，约 8s 就失败）、
`seep.eu.org` 挂 6 分多钟后 `OpenSSL SSL_read: Connection was reset (errno 10054)`、
`jsnzkpg4.pages.dev` **4.2s 推完**。顺序应该按当天实际情况动态选，别固定迷信
某一个；`down.nigx.cn` 的 429 是短时限流，等一会儿往往就恢复。
推完用 `git ls-remote <地址> refs/heads/main` 核对远端 SHA，再
`git update-ref refs/remotes/origin/main <sha>` 同步跟踪引用。

- **跨轮测量的噪声会骗人**：分别跑两轮 release probe 读 before/after，`full forward`
  涨了 11% 而没被改动的 `deep clone` 跌了 16%——加了一项交替测量才发现是负载噪声。
  现在 `perf_probe` 的同体对照是唯一可信口径。
- **`usize::from(u8)` 不能在 const 上下文里用**（`From` 还不是 const trait，rustc 1.95 报
  E0658）：`const` 表里只能用 `as` 转换。
- **文档注释不能挂在闭包 / `let` 上**：rustc 会报 unused doc comment，探针里的说明要用 `//`。
- **`#[global_allocator]` 放进 `#[cfg(test)]` 模块**：只作用于 `cargo test --lib` 的测试二进制，
  集成测试和正式构建不受影响，因此可以安全地在单元测试里做分配计数。
  `std::thread_local!` + `Cell`（const 初始化、无 Drop）避免分配器自己踩 TLS 拆卸。
- **`import type` 必须从模块图分析里剔掉**：我第一次写脚本统计「入口静态依赖」时没排它，
  把 `openclawProviderPresets`（213 KB）误判成首屏依赖，白折腾一轮。真实污染源只有
  `ProviderCard → copilotProviderPresets` 这一条。
