//! 「当前供应商」有两个：直连指针和代理路由，互相独立。
//!
//! 读当前供应商一律经 [`provider_for`]，并说明读它做什么。直接读直连指针
//! （`settings::get_effective_current_provider`、`Database::get_current_provider`）
//! 只允许在这里和它们自己的定义处，由下面的测试把关。

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::live::engine::DeviceStore;
use crate::provider::Provider;

use std::sync::Arc;

use super::state::{self, ModeState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// 直连指针：直连模式下写进客户端文件的那家，代理模式下退出代理时写回它。
    Direct,
    /// 请求实际发往的那家：代理模式下是代理路由，直连模式下是直连指针。界面上的
    /// 「当前」、用量、测速、项目快照都按它。
    InUse,
}

/// 这个应用的模式状态；没有路由模式的应用恒为直连。
pub fn mode_state(app: &AppType) -> ModeState {
    if !app.has_route_mode() {
        return ModeState::default();
    }
    state::mode_state(&DeviceStore::for_device(), app.as_str()).unwrap_or_else(|err| {
        log::warn!("读取 {} 的模式状态失败，按直连处理: {err}", app.as_str());
        ModeState::default()
    })
}

pub fn is_proxy(app: &AppType) -> bool {
    mode_state(app).is_proxy()
}

/// 同 [`is_proxy`]，一次看几个应用，状态文件只读一次（状态面板每 2 秒轮询一次）。
pub fn proxy_flags<const N: usize>(apps: [AppType; N]) -> [bool; N] {
    let names = apps.each_ref().map(AppType::as_str);
    match state::mode_states(&DeviceStore::for_device(), names) {
        Ok(states) => std::array::from_fn(|i| apps[i].has_route_mode() && states[i].is_proxy()),
        Err(err) => {
            log::warn!("读取模式状态失败，按直连处理: {err}");
            [false; N]
        }
    }
}

pub fn provider_for(
    db: &Database,
    app: &AppType,
    purpose: Purpose,
) -> Result<Option<String>, AppError> {
    if purpose == Purpose::InUse {
        if let Some(route) = proxy_route_row_shared(db, app)? {
            return Ok(Some(route.id.clone()));
        }
    }
    crate::settings::get_effective_current_provider(db, app)
}

/// 正在用的那一行（见 [`Purpose::InUse`]）。
pub fn provider_in_use(db: &Database, app: &AppType) -> Result<Option<Provider>, AppError> {
    Ok(provider_in_use_shared(db, app)?.map(|provider| provider.as_ref().clone()))
}

fn provider_in_use_shared(db: &Database, app: &AppType) -> Result<Option<Arc<Provider>>, AppError> {
    let mode = mode_state(app);
    provider_in_use_shared_with_state(db, app, mode)
}

/// 请求热路径的一次读取版本：模式状态和 Stack 开关来自同一个状态快照。
///
/// 设备目录由调用方给出：请求热路径每请求构造一次
/// [`DeviceStore`](crate::live::engine::DeviceStore) 交给它，和
/// [`provider_in_use_cached`] 共用同一个目录（测试也因此能完全隔离）。
pub fn provider_in_use_and_stack_mode(
    db: &Database,
    app: &AppType,
    store: &DeviceStore,
) -> (Option<Arc<Provider>>, bool) {
    if !app.has_route_mode() {
        return (direct_provider_shared(db, app).unwrap_or(None), false);
    }

    let (mode, stack_mode) = state::routing_state(store, app.as_str()).unwrap_or_else(|err| {
        log::warn!("读取 {} 的路由状态失败，按直连处理: {err}", app.as_str());
        (ModeState::default(), false)
    });
    let provider = provider_in_use_shared_with_state(db, app, mode).unwrap_or(None);
    (provider, stack_mode)
}

fn provider_in_use_shared_with_state(
    db: &Database,
    app: &AppType,
    mode: ModeState,
) -> Result<Option<Arc<Provider>>, AppError> {
    match proxy_route_row_shared_with_state(db, app, mode)? {
        Some(route) => Ok(Some(route)),
        None => direct_provider_shared(db, app),
    }
}

/// [`provider_in_use_and_stack_mode`] 的全缓存快照版：模式状态、Stack 开关和供应商行
/// 全部命中缓存（状态文件戳未变、providers 表代次未变、行 TTL 未过）时零 IO 返回。
///
/// 任何一项未命中返回 `None`，调用方回退到会读盘/查库的版本，语义与今天完全一致：
/// - providers 表的增删改都会抬代次，行被删必然未命中；
/// - 本地指针存在即赢（`is_current` 只是回退值），所以缓存行命中时无需再解析指针；
/// - 本地指针为空或行未缓存（含负缓存）都回退，由 `get_effective_current_provider`
///   做存在性校验和回退。
pub fn provider_in_use_cached(
    db: &Database,
    app: &AppType,
    store: &DeviceStore,
) -> Option<(Option<Arc<Provider>>, bool)> {
    if !app.has_route_mode() {
        let local = crate::settings::get_current_provider(app)?;
        let row = db.cached_shared_provider(&local, app.as_str())?;
        return Some((Some(row), false));
    }

    let (mode, stack_mode) = crate::mode::state::routing_state_cached(store, app.as_str())?;
    let route_or_pointer = if mode.is_proxy() {
        mode.proxy_route
            .or_else(|| crate::settings::get_current_provider(app))
    } else {
        crate::settings::get_current_provider(app)
    };
    let id = route_or_pointer?;
    let row = db.cached_shared_provider(&id, app.as_str())?;
    Some((Some(row), stack_mode))
}

fn proxy_route_row_shared_with_state(
    db: &Database,
    app: &AppType,
    mode: ModeState,
) -> Result<Option<Arc<Provider>>, AppError> {
    if !mode.is_proxy() {
        return Ok(None);
    }
    let Some(route) = mode.proxy_route else {
        return Ok(None);
    };
    let row = db.get_shared_provider_by_id(&route, app.as_str())?;
    if row.is_none() {
        log::warn!(
            "{} 的代理路由 {route} 在数据库中不存在，按直连指针处理",
            app.as_str()
        );
    }
    Ok(row)
}

fn proxy_route_row_shared(db: &Database, app: &AppType) -> Result<Option<Arc<Provider>>, AppError> {
    let mode = mode_state(app);
    proxy_route_row_shared_with_state(db, app, mode)
}

/// 直连指针指向的那一行。
pub fn direct_provider(db: &Database, app: &AppType) -> Result<Option<Provider>, AppError> {
    Ok(direct_provider_shared(db, app)?.map(|provider| provider.as_ref().clone()))
}

fn direct_provider_shared(db: &Database, app: &AppType) -> Result<Option<Arc<Provider>>, AppError> {
    match provider_for(db, app, Purpose::Direct)? {
        Some(id) => db.get_shared_provider_by_id(&id, app.as_str()),
        None => Ok(None),
    }
}

/// 设备本地记录的直连指针（不验证是否存在、不回落到 DB）。只给切换失败时的回滚用：
/// 回滚要把本地记录恢复成原样。
pub fn local_direct_pointer(app: &AppType) -> Option<String> {
    crate::settings::get_current_provider(app)
}

/// 删除前检查：直连指针（同 [`provider_for`] 的 `Direct`）或代理路由指着它，就算正在用。
///
/// 和界面上禁用删除的判断是同一个集合。不再单独看 DB 的 `is_current`：本地记录有效时
/// 它只是回退值，可能停在别的卡上（例如旧版 Claude Desktop 映射卡迁移后，本地指针改成了
/// 官方、`is_current` 还留在映射卡上），单独看它会让一张界面上可删的卡点了删除却被拒绝。
pub fn is_referenced(db: &Database, app: &AppType, id: &str) -> Result<bool, AppError> {
    if provider_for(db, app, Purpose::Direct)?.as_deref() == Some(id) {
        return Ok(true);
    }
    Ok(mode_state(app).routes_to(id))
}

#[cfg(test)]
mod tests {
    use super::state;
    use super::{provider_in_use_and_stack_mode, provider_in_use_cached};
    use crate::app_config::AppType;
    use crate::live::engine::DeviceStore;
    use std::fs;
    use std::path::Path;

    /// 直接读直连指针的地方：定义处、这里，以及只关心直连指针的写入路径。
    const ALLOWED: &[&str] = &[
        "settings.rs",
        "database/dao/providers.rs",
        "mode/current.rs",
    ];

    fn scan(dir: &Path, root: &Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, root, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if ALLOWED.contains(&relative.as_str()) {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            // 测试模块里造数据可以直接读写，只查生产代码：跳过顶层的
            // `#[cfg(test)] mod xxx { … }`（到下一个顶格的 `}` 为止）。
            let mut in_tests = false;
            let mut previous = "";
            for (index, line) in text.lines().enumerate() {
                if in_tests {
                    in_tests = line != "}";
                    previous = line;
                    continue;
                }
                if previous == "#[cfg(test)]" && line.starts_with("mod ") && line.ends_with('{') {
                    in_tests = true;
                    previous = line;
                    continue;
                }
                previous = line;
                if line.contains("get_effective_current_provider(")
                    || line.contains(".get_current_provider(")
                    || line.contains("settings::get_current_provider(")
                {
                    hits.push(format!("{relative}:{}: {}", index + 1, line.trim()));
                }
            }
        }
    }

    #[test]
    fn current_provider_is_read_through_provider_for() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        scan(&root, &root, &mut hits);
        assert!(
            hits.is_empty(),
            "读当前供应商要经 mode::current::provider_for 并说明用途（直连指针还是正在用的那家）:\n{}",
            hits.join("\n")
        );
    }

    /// 独立设备目录 + 内存库：代理路由指向的那家；代理模式下这条路径不读 settings。
    fn claude_proxy_route_fixture(
        route: &str,
    ) -> (
        std::sync::Arc<crate::database::Database>,
        DeviceStore,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("create device dir");
        let store = DeviceStore::at(dir.path());
        let provider = crate::provider::Provider::with_id(
            route.to_string(),
            format!("Provider {route}"),
            serde_json::json!({"original": true}),
            None,
        );
        state::update(&store, |live| {
            let app_state = live.apps.entry("claude".to_string()).or_default();
            app_state.mode = Some(state::Mode::Proxy);
            app_state.proxy_route = Some(route.to_string());
        })
        .expect("write proxy state");
        let db = std::sync::Arc::new(crate::database::Database::memory().expect("memory db"));
        db.save_provider("claude", &provider)
            .expect("save routed provider");
        // 代理路由读不到时冷路径要回退到 is_current 行：在库里标记，避免依赖
        // 全局 settings 里的直连指针（那是真实用户目录的状态）。
        db.set_current_provider("claude", route)
            .expect("mark provider current");
        // dir 跟着元组返回：状态缓存按路径隔离，测试结束目录删除、缓存项自然失配。
        (db, store, dir)
    }

    /// 稳态（状态文件戳、providers 表代次都没变）下缓存路径与冷路径选同一家；
    /// 行被删后代次抬升，缓存路径必须未命中、交给回退路径。
    #[test]
    fn provider_in_use_cached_matches_the_cold_path_until_a_write() {
        let (db, store, _dir) = claude_proxy_route_fixture("cached-hit");

        // 缓存按路径隔离，先经冷路径把状态和行都读进缓存，再走快路径。
        let cold = provider_in_use_and_stack_mode(&db, &AppType::Claude, &store);
        let cached = provider_in_use_cached(&db, &AppType::Claude, &store).expect("cached hit");
        assert_eq!(
            cached.0.as_ref().map(|provider| provider.id.as_str()),
            cold.0.as_ref().map(|provider| provider.id.as_str()),
        );
        assert_eq!(cached.1, cold.1);

        db.delete_provider("claude", "cached-hit")
            .expect("delete routed provider");
        assert!(
            provider_in_use_cached(&db, &AppType::Claude, &store).is_none(),
            "providers 表代次已变，缓存路径必须未命中并回退到查库",
        );
    }

    /// 外部直接改写 live-state.json（别的进程/手工编辑）：文件戳变了，
    /// 缓存路径未命中，回退路径读到新内容。
    #[test]
    fn provider_in_use_cached_misses_after_an_external_state_rewrite() {
        let (db, store, _dir) = claude_proxy_route_fixture("cached-hit");

        // 同上：先冷路径温缓存，快路径必须命中。
        let _warm = provider_in_use_and_stack_mode(&db, &AppType::Claude, &store);
        assert!(provider_in_use_cached(&db, &AppType::Claude, &store).is_some());

        fs::write(
            store.state_path(),
            r#"{"version": 1, "apps": {"claude": {"mode": "proxy", "proxy_route": "external"}}}"#,
        )
        .unwrap();

        // 外部写盘改了文件戳：快路径必须未命中，不能端出缓存里的旧快照。
        assert!(
            provider_in_use_cached(&db, &AppType::Claude, &store).is_none(),
            "状态文件戳已变，缓存路径必须未命中",
        );
        // 回退路径照样看得到外部写入的新路由（该路由不存在时落到直连指针）。
        let cold = provider_in_use_and_stack_mode(&db, &AppType::Claude, &store);
        assert_eq!(
            cold.0.as_ref().map(|provider| provider.id.as_str()),
            Some("cached-hit"),
        );
    }
}
