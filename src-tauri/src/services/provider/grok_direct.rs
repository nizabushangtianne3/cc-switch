//! 写 Grok Build 的 `config.toml`：只替换 `models.default` 和 CC Switch 写的那张模型表，
//! 其余字节不碰。
//!
//! 写 Grok live 的入口（切换、新增第一个供应商、编辑当前供应商、同步、统一供应商、
//! 进入 / 退出代理）都走这里：先拿应用写锁，再经 `mode::operation` 记下 pending，文件、
//! 指针和写入记录在同一个操作里提交。不回填：用户的设置和 MCP 本来就留在 live 里。

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::grok_config::get_grok_config_path;

use crate::live::engine::{DeviceStore, LiveFile};
use crate::live::patch::toml::{TomlDocPatch, TomlSteps};
use crate::live::project::claude::PROXY_TOKEN_PLACEHOLDER;
use crate::live::project::grok::{GrokConfigPatch, GrokProjection};
use crate::mode::operation::{AppWrite, FileChange, OperationReport};
use crate::mode::state::{self, op, PendingTarget, Written};
use crate::provider::Provider;

use super::editor_toml::TomlEdits;

fn app() -> &'static str {
    AppType::GrokBuild.as_str()
}

/// `~/.grok/config.toml`：模型表里有 Key，按 0600 写。
pub(crate) fn config_file() -> LiveFile {
    LiveFile::private(get_grok_config_path())
}

/// 官方卡：不写模型表，Grok 回落到内置模型和自带的 xAI 登录。
pub(crate) fn is_official(provider: &Provider) -> bool {
    provider.category.as_deref() == Some("official")
}

/// 供应商要写的模型表。写之前校验，校验不过什么都不写。
pub(crate) fn projection(provider: &Provider) -> Result<GrokProjection, AppError> {
    GrokProjection::of(&provider.settings_config, is_official(provider))
}

/// 上次写进 live 的表。有写入记录就按记录；这台设备上新版还没写过（刚从旧版升级）时，
/// 按旧版的写法推断：旧版把 `live_owner` 的行整份写进 live，它的 `models.default` 指的就是
/// CC Switch 的表。用户的其他表旧版也会原样写回，但证明不了是 CC Switch 的，不删。
pub(crate) fn retired_tables(
    store: &DeviceStore,
    live_owner: Option<&Provider>,
) -> Result<Vec<String>, AppError> {
    if let Some(written) = state::written(store, app())? {
        return Ok(written.tables);
    }
    let Some(owner) = live_owner.filter(|owner| !is_official(owner)) else {
        return Ok(Vec::new());
    };
    if let Ok(projection) = projection(owner) {
        return Ok(projection.written_tables());
    }
    // 行本身过不了校验（比如旧版回填把默认模型改成了内置的），就按行里写的名字删。
    Ok(owner
        .settings_config
        .get("config")
        .and_then(|config| config.as_str())
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("models")
                .and_then(|models| models.get("default"))
                .and_then(toml_edit::Item::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .into_iter()
        .collect())
}

/// 切到 `target`：同一个操作里写 live、改指针、记下写的表。`live_owner` 是 live 现在
/// 对应的供应商（直连指针那家），只在没有写入记录时用来推断旧版写的表。
pub(crate) fn switch_to(
    db: &Database,
    live_owner: Option<&Provider>,
    target: &Provider,
) -> Result<OperationReport, AppError> {
    write(db, op::SWITCH, live_owner, target, Some(&target.id))
}

/// 把当前供应商重新投影到 live，不改指针。
pub(crate) fn reapply(
    db: &Database,
    live_owner: Option<&Provider>,
    target: &Provider,
) -> Result<OperationReport, AppError> {
    write(db, op::APPLY, live_owner, target, None)
}

fn write(
    db: &Database,
    op: &str,
    live_owner: Option<&Provider>,
    target: &Provider,
    pointer: Option<&str>,
) -> Result<OperationReport, AppError> {
    let projection = projection(target)?;
    run(
        db,
        op,
        live_owner,
        Some(&projection),
        PendingTarget::pointer(pointer.map(str::to_string)),
    )
}

/// 把 `projection` 写进 `config.toml`，和 `target` 在同一个操作里提交，并记下写的表；
/// `projection` 为空时只落定状态、不读也不写文件。
pub(crate) fn run(
    db: &Database,
    op: &str,
    live_owner: Option<&Provider>,
    projection: Option<&GrokProjection>,
    target: PendingTarget,
) -> Result<OperationReport, AppError> {
    run_with_edits(db, op, live_owner, projection, target, None)
}

/// 同 [`run`]，另把编辑器保存的全局改动在同一次写入里写进去（先改动，后模型表）。
pub(crate) fn run_with_edits(
    db: &Database,
    op: &str,
    live_owner: Option<&Provider>,
    projection: Option<&GrokProjection>,
    mut target: PendingTarget,
    edits: Option<&TomlEdits>,
) -> Result<OperationReport, AppError> {
    // 先补完上一次没做完的操作：它可能改了写入记录和指针，下面要按最新的记录删表。
    let app_write = AppWrite::begin(db, app())?;
    let patch = match projection {
        Some(projection) => {
            target.written = Some(Written {
                tables: projection.written_tables(),
                ..Written::default()
            });
            Some(GrokConfigPatch::direct(
                projection,
                retired_tables(&app_write.store, live_owner)?,
                PROXY_TOKEN_PLACEHOLDER,
            ))
        }
        None => None,
    };
    let edits = edits.filter(|edits| !edits.is_empty());
    // 先按三方比较应用编辑器的改动，再换模型表。
    let mut steps: Vec<&dyn TomlDocPatch> = Vec::new();
    if let Some(edits) = edits {
        steps.push(edits);
    }
    if let Some(patch) = &patch {
        steps.push(patch);
    }
    let write = TomlSteps(steps);
    let changes: Vec<FileChange<'_>> = (!write.0.is_empty())
        .then(|| FileChange {
            file: config_file(),
            patch: &write,
        })
        .into_iter()
        .collect();
    app_write.run(op, &changes, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(name: &str) -> Provider {
        Provider {
            id: "grok-provider".to_string(),
            name: name.to_string(),
            settings_config: json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    /// 一份空的设备状态：`written` 读不到记录，正好走到旧版推断那条分支。
    fn fresh_store() -> (tempfile::TempDir, DeviceStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeviceStore::at(dir.path());
        (dir, store)
    }

    #[test]
    fn is_official_recognizes_only_the_official_category() {
        let mut provider = provider("Grok Build");
        assert!(!is_official(&provider));

        provider.category = Some("official".to_string());
        assert!(is_official(&provider));

        provider.category = Some("third-party".to_string());
        assert!(!is_official(&provider));
    }

    #[test]
    fn retired_tables_stays_empty_without_an_owner() {
        let (_dir, store) = fresh_store();
        assert!(retired_tables(&store, None).unwrap().is_empty());
    }

    #[test]
    fn retired_tables_ignores_an_official_owner() {
        let (_dir, store) = fresh_store();
        let mut owner = provider("Grok Build");
        owner.category = Some("official".to_string());
        owner.settings_config = json!({"config": "[models]\ndefault = \"grok-4-fast\"\n"});

        // 官方卡不写模型表，没有可收回的表。
        assert!(retired_tables(&store, Some(&owner)).unwrap().is_empty());
    }

    #[test]
    fn retired_tables_reads_the_legacy_table_out_of_the_owner_config() {
        let (_dir, store) = fresh_store();
        let mut owner = provider("Legacy Grok");
        // models.default 指向的表不存在，整份配置过不了投影校验，走旧版推断。
        owner.settings_config = json!({"config": "[models]\ndefault = \"grok-4-fast\"\n"});

        assert_eq!(
            retired_tables(&store, Some(&owner)).unwrap(),
            vec!["grok-4-fast".to_string()]
        );
    }

    #[test]
    fn retired_tables_ignores_a_blank_or_broken_legacy_config() {
        let (_dir, store) = fresh_store();

        let mut blank = provider("Blank Grok");
        blank.settings_config = json!({"config": "[models]\ndefault = \"   \"\n"});
        assert!(
            retired_tables(&store, Some(&blank)).unwrap().is_empty(),
            "空串表名不算数"
        );

        let mut broken = provider("Broken Grok");
        broken.settings_config = json!({"config": "this is not toml {{{"});
        assert!(
            retired_tables(&store, Some(&broken)).unwrap().is_empty(),
            "解析不了就不猜"
        );
    }
}
