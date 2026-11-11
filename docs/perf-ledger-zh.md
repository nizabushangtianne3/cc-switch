> 性能优化账本：cc-switch 的性能优化以此文件为唯一台账。
>
> 规则：每一个待优化项先在这里排队（带证据），落地后把实测数据补进来再改成「已完成」；
> 被证伪的做法直接删掉，不留传说。测量命令统一收在
> [`src-tauri/src/proxy/forwarder.rs`](src-tauri/src/proxy/forwarder.rs) 的 `perf_probe` 模块里。

# 性能优化账本

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

#### 试过并撤销：给规范化输出缓冲按原始长度预分配

- 想法：`canonicalize_json_string_if_parseable` 的 `canonical_json_string` 用
  `String::new()` 起步，倍增到几十 KB 要十几次 realloc、每次把已写内容整段拷一遍。
  而调用方手里就有原始文本——它的长度正是规范化输出的上界（键排序只改顺序、
  不改字节数），按它 `with_capacity` 就能省掉这段拷贝。
- 实测（debug、同进程同体、2000 轮、两条路径输出逐字节一致）：
  text=27 648 B → out=27 648 B，零容量 2502.06 µs、预分配 2449.56 µs，**-2.1%**。
- 结论：**撤销**。这段的成本几乎全在逐字符串转义与键排序上，倍增拷贝只占 0.2% 量级，
  为它加一个「带容量的第二入口」不划算。账本留数，下轮别再试。

#### 评估后不改：`providerConfigUtils.ts` 按「通用 / Codex TOML」拆开

- 动机：它在入口静态图里（`ProviderList` 引 `extractProviderBaseUrl`、`ProviderCard`
  引一组函数），而 1785 行里有约 1500 行是表单专用的 Codex TOML 读写。
- 卡点：**交叉依赖**。要留下来的 `extractProviderBaseUrl` 有一个分支要调
  `extractCodexBaseUrl`（Codex 组的导出），要么把它也搬去 Codex 组（那
  `ProviderList` 仍拖着整份 Codex 文件，收益归零），要么让通用模块反向依赖
  Codex 组（循环依赖）。
- 结论：不改。真正要做就得先解掉这个交叉（例如 `extractProviderBaseUrl` 的 TOML
  分支改成调用方注入），那是另一个任务。

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
- 附带把那条「顺带」也量了，免得下一轮再琢磨一遍：`needsLightBacking` 的取色判断
  热路径每轮（154 个 key 全查一遍）**0.041 ms**，冷路径（含 102 个 SVG 的解析与
  取色）是一次性的 1~5 ms 量级。把它固化成预计算名单只能省掉这两项里更小的那个，
  还要为一个 Map 换一个 Set —— 不做，已连同实测数一起记在这里。
- 顺带订正一条旧记录：`vite.config.ts` 的 `build.emptyOutDir` 不清 root 之外的
  目录（这里 root=`src`、outDir=`../dist`），跨实验的 chunk 文件会残留。凡是要
  「改动前 / 改动后」比字节的，必须先手工清空 dist 再构建，否则比的是两份不同
  的源码状态。上一版 manualChunks 的对比数就是这么错的，已按清空重测的数字订正。
- 状态：**待办**

## 落地清单（第一轮，提交 da72f940）

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

### 图标表诊断（P2-2）与热路径取价量化

- 用 `build.sourcemap` 开一次（分析完即关）拿到精确的模块归属，结论见 P2-2：
  懒加载界面的公共块 566 kB 里 143 个源文件中 54 个是 `src/icons`，
  `index.ts` 187 KB 源码 = 101 个内联 SVG 字面量共 178.8 KB；
  vendor 侧是 react-query / lucide / react-i18next / radix 这些真底座，砍不动。
- 顺手把账本里「顺带固化 needsLightBacking 名单」那条也量了再下结论：
  154 个 key 全查一遍的热路径 **0.041 ms**，冷路径（102 个 SVG 解析取色）一次性
  1~5 ms。固化名单只能省掉更小的那项，还要用一个 Set 换一个 Map——不做。
- 状态：**已诊断，不改**（P2-2 保留为待办，改动面大、收益薄）

### 测量口径订正

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
- `cargo test --tests`：218 passed / 0 failed
- `cargo clippy --lib --all-targets`：0 warning
- `cargo fmt --check`：干净
- `pnpm typecheck`、`pnpm format:check`、`pnpm build:renderer`：通过
- `pnpm vitest run`：4032 passed；唯一失败的 `App.test.tsx` 用例单独跑通过（3.7s），
  是改动前就有的执行时长 flake，不是回归

⚠️ 本地跑 `cargo nextest run` 会因为同时构建 16 个集成测试二进制而撞上 Windows
提交上限（错误 1455「页面文件太小」）。加 `-j 2` 分段构建就正常，CI 上跑在 Linux
不存在这个问题。

## 踩坑记录

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
