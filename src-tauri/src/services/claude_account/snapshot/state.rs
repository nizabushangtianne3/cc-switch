//! 本机状态文件（`<数据目录>/claude-account-snapshot.json`，不同步）和切换日志。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

const STATE_FILENAME: &str = "claude-account-snapshot.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// 最近一次切换的日志：没做完、或做完了还没确认卡片写成
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<Journal>,
    /// 不在默认槽位的账号（账号 key）登录什么时候过期（毫秒）：收回、登录时从凭据里记下来，
    /// 只用来提示，不是令牌。账号在自己的目录里被 Claude Code 刷新过时会偏早
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub login_expiry: BTreeMap<String, i64>,
}

/// 一次切换的日志，只有指纹和账号 key，没有令牌。按 [`Step`] 推进，确认后删除。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub target: String,
    pub target_dir: PathBuf,
    /// T 凭据里的 `organizationUuid`：提交之后被 Claude Code 刷新过（指纹变了），还靠它认出
    /// 默认槽位里仍是 T
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_org: Option<String>,
    /// T 的账号键的指纹：默认槽位里的账号键变成它，就说明提交点写进去了
    pub target_account: String,
    /// 写之前默认槽位里账号键的指纹（没有登录时为 `None`）
    pub before_account: Option<String>,
    /// 写之前默认槽位里的是哪个账号（没有登录时为 `None`）。恢复时用它区分「T 被 Claude Code
    /// 刷新过」和「用户之后登了同组织的别的号」；卡片没写成时靠它换回
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_owner: Option<String>,
    pub step: Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    /// 已收回 O，准备写默认槽位（提交点之前）
    Parked,
    /// 默认槽位已写成 T，还要改 `.claude.json`、清空 T 的槽位
    Committed,
    /// 登录这边做完了，等调用方写完卡片指针和 settings.json 后确认。启动时还在这一步，
    /// 说明崩在中间：按卡片指针决定留着还是换回
    Done,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILENAME)
}

pub fn load_state(data_dir: &Path) -> State {
    std::fs::read(state_path(data_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn save_state(data_dir: &Path, state: &State) -> Result<(), AppError> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|source| AppError::JsonSerialize { source })?;
    crate::config::atomic_write(&state_path(data_dir), &bytes)
}

/// 状态文件的读改写互斥：切换、恢复和登录完成各改各的字段，不能拿旧的整份覆盖对方。
static STATE: Mutex<()> = Mutex::new(());

/// 在锁里重读状态、改、落盘，返回改后的状态。
pub fn update_state(data_dir: &Path, f: impl FnOnce(&mut State)) -> Result<State, AppError> {
    let _guard = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut state = load_state(data_dir);
    f(&mut state);
    save_state(data_dir, &state)?;
    Ok(state)
}

/// 调用方写完卡片（或已决定回滚方向）后调用：删掉做完的切换日志。没做完的留着给恢复。
pub fn confirm(data_dir: &Path) {
    let done = |state: &State| {
        state
            .journal
            .as_ref()
            .is_some_and(|journal| journal.step == Step::Done)
    };
    if !done(&load_state(data_dir)) {
        return;
    }
    let result = update_state(data_dir, |state| {
        if done(state) {
            state.journal = None;
        }
    });
    if let Err(error) = result {
        log::warn!("[ClaudeAccount] 清除切换日志失败: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(step: Step) -> Journal {
        Journal {
            target: "work".to_string(),
            target_dir: PathBuf::from("/tmp/work"),
            target_org: Some("org-1".to_string()),
            target_account: "fp-target".to_string(),
            before_account: Some("fp-before".to_string()),
            before_owner: Some("before-owner".to_string()),
            step,
        }
    }

    #[test]
    fn a_missing_or_corrupt_state_file_reads_as_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let state = load_state(dir.path());
        assert!(state.journal.is_none());
        assert!(state.login_expiry.is_empty());

        std::fs::write(state_path(dir.path()), "{not json").unwrap();
        let state = load_state(dir.path());
        assert!(state.journal.is_none());
        assert!(state.login_expiry.is_empty());
    }

    #[test]
    fn the_journal_round_trips_through_camel_case_json() {
        let dir = tempfile::tempdir().unwrap();
        let saved = State {
            journal: Some(journal(Step::Committed)),
            login_expiry: BTreeMap::from([("work".to_string(), 1_700_000_000_000)]),
        };
        update_state(dir.path(), |state| *state = saved.clone()).unwrap();

        let raw = std::fs::read_to_string(state_path(dir.path())).unwrap();
        assert!(raw.contains("\"targetOrg\""), "{raw}");
        assert!(raw.contains("\"beforeAccount\""), "{raw}");
        assert!(raw.contains("\"loginExpiry\""), "{raw}");

        let loaded = load_state(dir.path());
        assert_eq!(loaded.journal, saved.journal);
        assert_eq!(loaded.login_expiry, saved.login_expiry);
    }

    #[test]
    fn an_empty_expiry_map_is_omitted_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        update_state(dir.path(), |state| {
            state.login_expiry.insert("work".to_string(), 42);
        })
        .unwrap();
        let raw = std::fs::read_to_string(state_path(dir.path())).unwrap();
        assert!(raw.contains("loginExpiry"), "{raw}");

        update_state(dir.path(), |state| state.login_expiry.clear()).unwrap();
        let raw = std::fs::read_to_string(state_path(dir.path())).unwrap();
        assert!(!raw.contains("loginExpiry"), "{raw}");
    }

    #[test]
    fn confirm_clears_a_finished_switch_and_keeps_an_unfinished_one() {
        let dir = tempfile::tempdir().unwrap();

        // 没有日志：什么都不做，也不建文件。
        confirm(dir.path());
        assert!(!state_path(dir.path()).exists());

        for step in [Step::Parked, Step::Committed] {
            update_state(dir.path(), |state| state.journal = Some(journal(step))).unwrap();
            confirm(dir.path());
            let loaded = load_state(dir.path());
            assert_eq!(
                loaded.journal.map(|journal| journal.step),
                Some(step),
                "step {step:?} must survive confirm so recovery can finish it"
            );
        }

        update_state(dir.path(), |state| {
            state.journal = Some(journal(Step::Done))
        })
        .unwrap();
        confirm(dir.path());
        let loaded = load_state(dir.path());
        assert!(loaded.journal.is_none(), "a done switch must be cleared");
    }

    #[test]
    fn confirm_keeps_the_expiry_map_when_it_drops_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        update_state(dir.path(), |state| {
            state.journal = Some(journal(Step::Done));
            state.login_expiry.insert("work".to_string(), 7);
        })
        .unwrap();

        confirm(dir.path());

        let loaded = load_state(dir.path());
        assert!(loaded.journal.is_none());
        assert_eq!(loaded.login_expiry.get("work"), Some(&7));
    }
}
