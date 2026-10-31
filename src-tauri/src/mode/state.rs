//! `live-state.json`：这台设备上每个应用的客户端文件状态。
//!
//! - `mode`、`attached`、`proxy_route`、`contract`：直连 / 代理模式（`mode::controller`）；
//! - `written`：CC Switch 上次写进客户端文件、之后要按记录删掉的东西（Grok 的模型表）；
//! - `stack`：代理模式的 Stack 模型（`mode::stack`）；
//! - `pending`：一次写客户端文件的操作在发布前写下的意图，按文件记录写前、写后的
//!   hash 和已备好的临时文件，崩溃后据此前滚或丢弃（`mode::operation`）。
//!
//! 不认识的字段读写时原样保留。
//!
//! 文件是设备本地的（0600，不同步），路径见 [`DeviceStore`]。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::stage_write;
use crate::error::AppError;
use crate::live::engine::DeviceStore;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveState {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub apps: BTreeMap<String, AppLiveState>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn state_version() -> u32 {
    STATE_VERSION
}

impl Default for LiveState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            apps: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Direct,
    Proxy,
}

/// 进入代理时写进客户端文件的契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contract {
    pub version: u32,
    /// 契约内容的摘要：切换路由时摘要相同，客户端文件就不用动。
    pub key: String,
    /// 契约写进客户端的独有字段。退出代理时按它删除（值相同才删）：路由供应商的行之后可能
    /// 被编辑过，不能到时再按行重新计算。
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub exclusive: Map<String, Value>,
}

/// 一个应用的模式状态。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModeState {
    /// 没有值：这台设备还没运行过有双模式的版本，启动时按旧版遗留的接管状态定下来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// 客户端文件当前是否指向代理。退出 CC Switch 时分离、下次启动再接上。
    #[serde(default, skip_serializing_if = "is_false")]
    pub attached: bool,
    /// 代理模式下路由到的供应商。和直连指针互相独立，退出代理时保留。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<Contract>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ModeState {
    pub fn is_proxy(&self) -> bool {
        self.mode == Some(Mode::Proxy)
    }

    /// 代理模式下路由到的就是 `id` 这家。
    pub fn routes_to(&self, id: &str) -> bool {
        self.is_proxy() && self.proxy_route.as_deref() == Some(id)
    }
}

/// CC Switch 上次写进客户端文件、切走时要按记录删掉的东西。
///
/// 不能按 live 现在的内容去找：客户端自己会改。Grok 的 `/settings` 会把 `models.default`
/// 改成内置模型，按它找表就会漏删上一家的表；而 CC Switch 默认的表名 `grok-4.5` 正好是
/// 内置模型 ID，留下的表会覆盖内置模型，把官方请求连同第三方 Key 发到第三方地址。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Written {
    /// Grok Build `config.toml` 里 CC Switch 写的 `[model."<名称>"]` 表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 代理模式的 Stack 模型：这些供应商的模型以带前缀的 id 发布给客户端，选中后请求直达那一家
/// （`mode::stack`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StackState {
    /// Stack 模式：代理模式下发布 Stack 模型、不做故障转移（界面上和路由模式二选一，见
    /// `controller::enter`）。只在代理模式下有意义：每次进入代理时按用户选的模式写定，
    /// 退出代理时不动，名单也留着。
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    /// 当前 Stack 里的供应商 id，按加入顺序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// key 登记簿：key → 供应商 id。一经分配永久归这家，移除成员、删除供应商都不回收：
    /// 客户端会一直带着选中过的 id，key 改了指向，旧 id 就会被悄悄发到另一家。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl StackState {
    pub fn is_empty(&self) -> bool {
        !self.enabled && self.members.is_empty() && self.keys.is_empty() && self.extra.is_empty()
    }

    /// 这家在登记簿里的 key。
    pub fn key_of(&self, provider_id: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(_, id)| id.as_str() == provider_id)
            .map(|(key, _)| key.as_str())
    }

    pub fn is_member(&self, provider_id: &str) -> bool {
        self.members.iter().any(|id| id == provider_id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppLiveState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub attached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<Contract>,
    /// 没有值：这台设备上还没有新版写过这个应用的文件（升级前旧版写的，按行推断）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<Written>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
    #[serde(default, skip_serializing_if = "StackState::is_empty")]
    pub stack: StackState,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl AppLiveState {
    fn is_empty(&self) -> bool {
        self.pending.is_none()
            && self.written.is_none()
            && self.stack.is_empty()
            && self.mode_state() == ModeState::default()
            && self.extra.is_empty()
    }

    pub fn mode_state(&self) -> ModeState {
        ModeState {
            mode: self.mode,
            attached: self.attached,
            proxy_route: self.proxy_route.clone(),
            contract: self.contract.clone(),
        }
    }

    pub fn set_mode_state(&mut self, state: ModeState) {
        self.mode = state.mode;
        self.attached = state.attached;
        self.proxy_route = state.proxy_route;
        self.contract = state.contract;
    }
}

/// 操作名。用字符串而不是枚举，旧版本读到新版本写的操作名也能照常前滚或丢弃。
pub mod op {
    /// 切换供应商（会改指针）。
    pub const SWITCH: &str = "switch";
    /// 把当前供应商重新写进客户端文件（编辑、同步等）。
    pub const APPLY: &str = "apply";
    /// 进入代理模式：客户端文件写成代理契约。
    pub const ENTER: &str = "enter";
    /// 退出代理模式：客户端文件写回直连供应商。
    pub const EXIT: &str = "exit";
    /// 退出 CC Switch 时把客户端指回直连，模式不变。
    pub const DETACH: &str = "detach";
    /// 启动时把客户端重新指向代理。
    pub const ATTACH: &str = "attach";
    /// 代理模式下换路由（契约变了时同一操作里先改写客户端）。
    pub const ROUTE: &str = "route";
    /// 增删 Stack 模型（契约变了时同一操作里先改写客户端）。
    pub const STACK: &str = "stack";
    /// Codex 改用 CC Switch 生成的模型目录（用户在 Stack 提示上点的）：一律重写客户端。
    pub const CATALOG: &str = "catalog";
}

/// 一次操作的写前意图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub op: String,
    pub files: Vec<PendingFile>,
    #[serde(default)]
    pub target: PendingTarget,
    /// 已经开始发布：换进第一个文件之前记下，所以只要有文件发布过它就一定在。发布过的
    /// 文件之后可能又被客户端改掉（Codex 刷新登录），单看文件内容就分不出发布开始过没有，
    /// 恢复时靠它决定前滚还是丢弃。
    #[serde(default, skip_serializing_if = "is_false")]
    pub published: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingFile {
    pub path: PathBuf,
    /// 写前内容的 hash；`None` 表示写前文件不存在。
    pub pre: Option<String>,
    /// 写后内容的 hash；`None` 表示这个操作要删掉它。
    #[serde(default)]
    pub planned: Option<String>,
    /// 已写好写后内容、等着 rename 的临时文件；删文件时没有。
    #[serde(default)]
    pub staged: Option<PathBuf>,
}

/// 文件都写完之后要落定的状态。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingTarget {
    /// 直连指针：切换成功后当前供应商是谁。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// 模式状态：有值时整体替换这个应用的 mode、attached、proxy_route、contract。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ModeState>,
    /// 写入记录：有值时整体替换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<Written>,
    /// Stack 模型：有值时整体替换这个应用的 `stack`（成员和登记簿一起）。不放进 `state`：
    /// `state` 会整体替换，不认识 `stack` 的版本写下的 pending 前滚时就会把名单清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<StackState>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PendingTarget {
    /// 只改直连指针（`None` 表示不改）。
    pub fn pointer(pointer: Option<String>) -> Self {
        Self {
            pointer,
            ..Self::default()
        }
    }

    /// 只落定模式状态。
    pub fn mode(state: ModeState) -> Self {
        Self {
            state: Some(state),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pointer.is_none()
            && self.state.is_none()
            && self.written.is_none()
            && self.stack.is_none()
            && self.extra.is_empty()
    }
}

/// 状态文件是所有应用共用的，读改写要串行。
fn state_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

/// 读状态文件。不存在时是空状态；内容坏了就挪到一旁（`live-state.json.corrupt-<时间>`）
/// 从空状态开始：这是 CC Switch 自己的文件，里面只有未完成操作的意图。
pub fn load(store: &DeviceStore) -> Result<LiveState, AppError> {
    let path = store.state_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(LiveState::default()),
        Err(source) => return Err(AppError::io(&path, source)),
    };
    match serde_json::from_slice::<LiveState>(&bytes) {
        Ok(state) => Ok(state),
        Err(err) => {
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            let aside = path.with_file_name(format!("live-state.json.corrupt-{stamp}"));
            log::warn!(
                "live-state.json 无法解析（{err}），已移到 {} 并从空状态开始",
                aside.display()
            );
            fs::rename(&path, &aside).map_err(|source| AppError::io(&path, source))?;
            Ok(LiveState::default())
        }
    }
}

fn save(store: &DeviceStore, state: &LiveState) -> Result<(), AppError> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|source| AppError::JsonSerialize { source })?;
    stage_write(&store.state_path(), &bytes, Some(0o600), true)?.commit()?;
    // 自己刚写出去的就是最新鲜的内容：缓存直接对准它，后续读到本次写入，
    // 不用等下一次文件戳校验把陈旧条目刷新掉。
    cache_replace(&store.state_path(), Arc::new(state.clone()));
    Ok(())
}

/// 一个路径上的状态缓存：命中条件是路径 + 文件戳（mtime、长度）都没变。
///
/// 代理热路径每个请求都要读一次状态（`provider_in_use_and_stack_mode` ->
/// [`routing_state`]），读盘加 JSON 解析每请求都白付一次；这里把它降到一次
/// `stat` 加一个 `Arc` 克隆。
#[derive(Clone)]
struct CachedState {
    stamp: FileStamp,
    state: Arc<LiveState>,
}

#[derive(Clone, Copy, PartialEq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    fn of(path: &std::path::Path) -> Self {
        match fs::metadata(path) {
            Ok(meta) => Self {
                modified: meta.modified().ok(),
                len: meta.len(),
            },
            // 文件不存在：u64::MAX 的长度让这个戳匹配不上任何真实文件。
            Err(_) => Self {
                modified: None,
                len: u64::MAX,
            },
        }
    }
}

/// 按路径各存一份。一个进程只有一个设备目录，生产行为和「只存一条、读谁替换谁」
/// 完全一致；各路径独立后，并发的读者不会因为别的路径写缓存而丢自己的条目。
fn state_cache() -> &'static Mutex<HashMap<PathBuf, CachedState>> {
    static CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedState>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &CACHE
}

/// 读一次缓存条目，不含戳校验：临界区内只克隆 `Arc` 与路径，`stat` 放到锁外，
/// 并发请求不会在文件系统调用上互相排队。
fn cache_take(path: &std::path::Path) -> Option<CachedState> {
    let cache = state_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.get(path).cloned()
}

fn cache_replace(path: &std::path::Path, state: Arc<LiveState>) {
    let mut cache = state_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(
        path.to_path_buf(),
        CachedState {
            stamp: FileStamp::of(path),
            state,
        },
    );
}

/// [`load`] 的缓存版：进程内同一路径只读盘一次，之后每次命中做一次 `stat`
/// 校验；文件变了（外部写入或别处改名）或我们自己的写盘会自然失配重读。
///
/// 返回 `Arc<LiveState>`：命中只抬一次引用计数。热路径每个请求都要读一次
/// 状态，深拷贝整棵状态树（每应用的模式、契约、Stack、pending、extra）的
/// 开销曾经全花在克隆上。
fn load_cached_shared(store: &DeviceStore) -> Result<Arc<LiveState>, AppError> {
    let path = store.state_path();
    if let Some(entry) = cache_take(&path).filter(|entry| entry.stamp == FileStamp::of(&path)) {
        return Ok(entry.state);
    }
    let state = Arc::new(load(store)?);
    cache_replace(&path, Arc::clone(&state));
    Ok(state)
}

/// 读改写状态文件。
pub fn update<R>(
    store: &DeviceStore,
    change: impl FnOnce(&mut LiveState) -> R,
) -> Result<R, AppError> {
    let _guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut state = load(store)?;
    let result = change(&mut state);
    state.apps.retain(|_, app| !app.is_empty());
    save(store, &state)?;
    Ok(result)
}

pub fn pending(store: &DeviceStore, app: &str) -> Result<Option<Pending>, AppError> {
    let _guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    Ok(load(store)?
        .apps
        .get(app)
        .and_then(|state| state.pending.clone()))
}

pub fn set_pending(
    store: &DeviceStore,
    app: &str,
    pending: Option<Pending>,
) -> Result<(), AppError> {
    update(store, |state| {
        state.apps.entry(app.to_string()).or_default().pending = pending;
    })
}

/// 这个应用的模式状态。
/// 几个应用的模式状态，状态文件只读一次。
pub fn mode_states<const N: usize>(
    store: &DeviceStore,
    apps: [&str; N],
) -> Result<[ModeState; N], AppError> {
    // 状态面板每 2 秒轮询一次：走同一份按文件戳失效的缓存，不再每次读盘解析，
    // 也不为读持 `state_lock`（读-改-写才需要它，见 `update`）。
    let state = load_cached_shared(store)?;
    Ok(apps.map(|app| {
        state
            .apps
            .get(app)
            .map(AppLiveState::mode_state)
            .unwrap_or_default()
    }))
}

pub fn mode_state(store: &DeviceStore, app: &str) -> Result<ModeState, AppError> {
    let [mode] = mode_states(store, [app])?;
    Ok(mode)
}

/// 这个应用的写入记录；`None` 表示新版还没写过。
pub fn written(store: &DeviceStore, app: &str) -> Result<Option<Written>, AppError> {
    let _guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    Ok(load(store)?
        .apps
        .get(app)
        .and_then(|state| state.written.clone()))
}

/// 这个应用的 Stack 模型（成员和 key 登记簿）。
pub fn stack(store: &DeviceStore, app: &str) -> Result<StackState, AppError> {
    let _guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    Ok(load(store)?
        .apps
        .get(app)
        .map(|state| state.stack.clone())
        .unwrap_or_default())
}

/// 这个应用在 Stack 模式（代理模式且 Stack 模式开着），状态文件只读一次。
pub fn stack_mode(store: &DeviceStore, app: &str) -> Result<bool, AppError> {
    let _guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    Ok(load(store)?
        .apps
        .get(app)
        .is_some_and(|state| state.mode == Some(Mode::Proxy) && state.stack.enabled))
}

/// 一次读出路由决策需要的模式状态和 Stack 开关；状态文件只读一次。
pub fn routing_state(store: &DeviceStore, app: &str) -> Result<(ModeState, bool), AppError> {
    // 代理每个请求都过这里。写盘走临时文件原子替换，读端看到的永远是完整的
    // 新旧状态之一，所以热路径读不再持 `state_lock`（那把锁只服务于
    // `update` 的读-改-写）；缓存按文件戳自然失效，命中只抬一次引用计数。
    let state = load_cached_shared(store)?;
    let app_state = state.apps.get(app);
    let mode = app_state.map(AppLiveState::mode_state).unwrap_or_default();
    let stack_mode =
        app_state.is_some_and(|state| state.mode == Some(Mode::Proxy) && state.stack.enabled);
    Ok((mode, stack_mode))
}

/// [`routing_state`] 的全缓存版：命中时只做一次 `stat` 比对文件戳，不读盘、不解析；
/// 未命中（缓存为空、换了路径、文件戳变了）返回 `None`，调用方回退到会读盘的版本。
///
/// 代理热路径每个请求都要拿这两位开关，缓存命中是稳态；未命中回退后由
/// [`load_cached_shared`] 把新内容补进缓存。
pub fn routing_state_cached(store: &DeviceStore, app: &str) -> Option<(ModeState, bool)> {
    let path = store.state_path();
    let entry = cache_take(&path)?;
    if entry.stamp != FileStamp::of(&path) {
        return None;
    }
    let app_state = entry.state.apps.get(app);
    let mode = app_state.map(AppLiveState::mode_state).unwrap_or_default();
    let stack_mode =
        app_state.is_some_and(|state| state.mode == Some(Mode::Proxy) && state.stack.enabled);
    Some((mode, stack_mode))
}

/// 有未完成操作的应用。
pub fn apps_with_pending(store: &DeviceStore) -> Result<Vec<String>, AppError> {
    let _guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    Ok(load(store)?
        .apps
        .into_iter()
        .filter(|(_, state)| state.pending.is_some())
        .map(|(app, _)| app)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_pending() -> Pending {
        Pending {
            op: op::SWITCH.to_string(),
            files: vec![PendingFile {
                path: PathBuf::from("/tmp/settings.json"),
                pre: None,
                planned: Some("abc".to_string()),
                staged: Some(PathBuf::from("/tmp/settings.json.tmp.1")),
            }],
            target: PendingTarget::pointer(Some("p1".to_string())),
            published: false,
        }
    }

    #[test]
    fn pending_round_trips_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());

        assert_eq!(pending(&store, "claude").unwrap(), None);
        set_pending(&store, "claude", Some(sample_pending())).unwrap();
        assert_eq!(pending(&store, "claude").unwrap(), Some(sample_pending()));
        assert_eq!(
            apps_with_pending(&store).unwrap(),
            vec!["claude".to_string()]
        );

        set_pending(&store, "claude", None).unwrap();
        assert_eq!(pending(&store, "claude").unwrap(), None);
        let raw: Value = serde_json::from_slice(&fs::read(store.state_path()).unwrap()).unwrap();
        assert_eq!(raw, json!({"version": 1, "apps": {}}));
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());
        fs::write(
            store.state_path(),
            r#"{"version": 2, "future": true, "apps": {"codex": {"mode": "proxy"}}}"#,
        )
        .unwrap();

        set_pending(&store, "claude", Some(sample_pending())).unwrap();
        set_pending(&store, "claude", None).unwrap();

        let raw: Value = serde_json::from_slice(&fs::read(store.state_path()).unwrap()).unwrap();
        assert_eq!(
            raw,
            json!({"version": 2, "future": true, "apps": {"codex": {"mode": "proxy"}}})
        );
    }

    #[test]
    fn routing_state_reads_mode_and_stack_from_one_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());

        assert_eq!(
            routing_state(&store, "claude").unwrap(),
            (ModeState::default(), false)
        );

        let mut state = load(&store).unwrap();
        let app_state = state.apps.entry("claude".to_string()).or_default();
        app_state.mode = Some(Mode::Proxy);
        app_state.proxy_route = Some("provider".to_string());
        app_state.stack = StackState {
            enabled: true,
            ..StackState::default()
        };
        save(&store, &state).unwrap();

        assert_eq!(
            routing_state(&store, "claude").unwrap(),
            (
                ModeState {
                    mode: Some(Mode::Proxy),
                    proxy_route: Some("provider".to_string()),
                    ..ModeState::default()
                },
                true
            )
        );
    }

    /// 缓存命中之后，外部直接改文件（别的进程、手工编辑）必须仍能被下一次读到：
    /// 文件戳（mtime/len）变了就整条重读，缓存不是「读一次就钉死」。
    #[test]
    fn routing_state_cache_sees_external_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());

        assert_eq!(
            routing_state(&store, "claude").unwrap(),
            (ModeState::default(), false)
        );

        // 外部写入：不经 update/save，直接落盘
        fs::write(
            store.state_path(),
            r#"{"version": 1, "apps": {"claude": {"mode": "proxy", "proxy_route": "external"}}}"#,
        )
        .unwrap();

        let (mode, stack) = routing_state(&store, "claude").unwrap();
        assert_eq!(mode.proxy_route.as_deref(), Some("external"));
        assert!(!stack);
        // 再读一次（此时命中缓存）不能把刚看到的外部内容又变回旧的
        assert_eq!(
            routing_state(&store, "claude")
                .unwrap()
                .0
                .proxy_route
                .as_deref(),
            Some("external")
        );
    }

    /// 自己写盘（save/update）之后立刻读，必须看到刚才写的内容。
    #[test]
    fn routing_state_cache_sees_own_write_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());

        assert_eq!(
            routing_state(&store, "claude").unwrap(),
            (ModeState::default(), false)
        );

        let mut state = load(&store).unwrap();
        let app_state = state.apps.entry("claude".to_string()).or_default();
        app_state.mode = Some(Mode::Proxy);
        app_state.proxy_route = Some("own-write".to_string());
        save(&store, &state).unwrap();

        assert_eq!(
            routing_state(&store, "claude")
                .unwrap()
                .0
                .proxy_route
                .as_deref(),
            Some("own-write")
        );
    }

    /// 缓存按路径隔离：两个设备目录交替读，互不串味。
    #[test]
    fn state_cache_is_keyed_by_path() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let store_a = DeviceStore::at(dir_a.path());
        let store_b = DeviceStore::at(dir_b.path());

        fs::write(
            store_a.state_path(),
            r#"{"version": 1, "apps": {"claude": {"mode": "proxy", "proxy_route": "a"}}}"#,
        )
        .unwrap();
        fs::write(
            store_b.state_path(),
            r#"{"version": 1, "apps": {"claude": {"mode": "proxy", "proxy_route": "b"}}}"#,
        )
        .unwrap();

        assert_eq!(
            routing_state(&store_a, "claude")
                .unwrap()
                .0
                .proxy_route
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            routing_state(&store_b, "claude")
                .unwrap()
                .0
                .proxy_route
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            routing_state(&store_a, "claude")
                .unwrap()
                .0
                .proxy_route
                .as_deref(),
            Some("a")
        );
    }

    /// 状态面板轮询走的 `mode_states` 与热路径共用同一份按文件戳失效的缓存：
    /// 外部直接改文件后，下一次轮询必须读到新内容，而不是缓存里的旧快照。
    #[test]
    fn mode_states_cache_sees_external_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());

        assert!(!mode_states(&store, ["claude"]).unwrap()[0].is_proxy());

        fs::write(
            store.state_path(),
            r#"{"version": 1, "apps": {"claude": {"mode": "proxy", "proxy_route": "polled"}}}"#,
        )
        .unwrap();

        let modes = mode_states(&store, ["claude", "codex"]).unwrap();
        assert_eq!(modes[0].proxy_route.as_deref(), Some("polled"));
        assert!(!modes[1].is_proxy());
    }

    #[test]
    fn a_corrupt_state_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());
        fs::write(store.state_path(), "{ not json").unwrap();

        assert_eq!(load(&store).unwrap(), LiveState::default());
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("live-state.json.corrupt-")),
            "{names:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());
        set_pending(&store, "claude", Some(sample_pending())).unwrap();
        let mode = fs::metadata(store.state_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
