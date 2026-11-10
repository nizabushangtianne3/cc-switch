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
- 实测（release，20 000 轮）：`dirs::home_dir` 0.610 µs/次，
  `config::get_home_dir` 2.800 µs/次，即每次请求白付约 2.2 µs + 2 次分配。
  纯透传一次 forward 约 0.92 ms，占约 0.3%。
- 方案：进程级缓存设备根，按 `CC_SWITCH_TEST_HOME` 的当前值失效；
  `settings.rs` 已有同一环境变量的 RwLock 快照，可以合过来。
- 状态：**待办**（收益小、要动测试切 HOME 的路径，排在前四项之后）

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

- `src-tauri/src/proxy/forwarder.rs` 10 134 行，`forward()` 单函数约 1 700 行；
  `handlers.rs` 4 988 行。编译单元和 I-cache 压力大，LLVM 的跨函数优化空间受限。
- 配置模块族：claude / codex / dsh / hermes / kimi / mcode / omp / openclaw / opencode /
  pi / agy / copilot / gemini / grok / claude_desktop 十几份近同构读写处理器
  （codex_config.rs 一份 268 KB）。
- 前端懒加载块仍偏大：`AuthSettingsPanel-*.js` 846.60 kB；
  `pnpm build:renderer` 有 6 个块 > 500 kB（main 655 kB、disableContextMenu 580 kB、
  index 444 kB、UsageTrendChart 360 kB…）。
- `App.tsx` 静态引入 `useToolManagement.ts`（1 088 行 / 42 KB）只为拿
  `checkToolUpdatesInBackground` 和 `useToolUpdatesAvailable` 两个小函数，
  整个「应用」页的安装/升级/卸载逻辑因此进了入口块。这两个函数依赖模块级
  toolVersionsCache，要拆就得连 store 一起搬，单独排期。
- 入口块仍静态拖着 `src/icons/extracted/index.ts`（187 KB 源码，约 126 个图标文件）：
  大 PNG 由 Vite 作为独立资源发出，JS 侧开销是 URL 表，暂无收益，先记为未决。
- 状态：**待办**

## 本轮落地清单

| # | 项 | 关键改动 | 验证 |
| --- | --- | --- | --- |
| 1 | P0-1 | `write_json_string` 就地转义 + `SmallVec` 键表 | 5005 + 9 个规范化测试全绿；分配计数随字符串数脱钩 |
| 2 | P0-2 | 拆出 `src/config/copilotEnv.ts` | 入口块 -88.11 kB；typecheck + 4 个预设测试 |
| 3 | P0-3 | 合并/删除 SSE 扫描同构实现 | sse 测试不变，全量测试通过 |
| 4 | P0-4 | xAI 整流透传分支一次分配 | 该文件测试不变 |

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
