//! 提示词数据访问对象
//!
//! 提供提示词（Prompt）的 CRUD 操作。

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::prompt::Prompt;
use indexmap::IndexMap;
use rusqlite::params;

impl Database {
    /// 获取指定应用类型的所有提示词
    pub fn get_prompts(&self, app_type: &str) -> Result<IndexMap<String, Prompt>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT id, name, content, description, enabled, created_at, updated_at
             FROM prompts WHERE app_type = ?1
             ORDER BY created_at ASC, id ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let prompt_iter = stmt
            .query_map(params![app_type], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let content: String = row.get(2)?;
                let description: Option<String> = row.get(3)?;
                let enabled: bool = row.get(4)?;
                let created_at: Option<i64> = row.get(5)?;
                let updated_at: Option<i64> = row.get(6)?;

                Ok((
                    id.clone(),
                    Prompt {
                        id,
                        name,
                        content,
                        description,
                        enabled,
                        created_at,
                        updated_at,
                    },
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut prompts = IndexMap::new();
        for prompt_res in prompt_iter {
            let (id, prompt) = prompt_res.map_err(|e| AppError::Database(e.to_string()))?;
            prompts.insert(id, prompt);
        }
        Ok(prompts)
    }

    /// 保存提示词
    pub fn save_prompt(&self, app_type: &str, prompt: &Prompt) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO prompts (
                id, app_type, name, content, description, enabled, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                prompt.id,
                app_type,
                prompt.name,
                prompt.content,
                prompt.description,
                prompt.enabled,
                prompt.created_at,
                prompt.updated_at,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    // Backfill and activation must commit together with MiniMax Code's live file.
    pub(crate) fn save_mcode_prompts(
        &self,
        prompts: &IndexMap<String, Prompt>,
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let transaction = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        for prompt in prompts.values() {
            transaction
                .execute(
                    "INSERT OR REPLACE INTO prompts (
                    id, app_type, name, content, description, enabled, created_at, updated_at
                ) VALUES (?1, 'mcode', ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        prompt.id,
                        prompt.name,
                        prompt.content,
                        prompt.description,
                        prompt.enabled,
                        prompt.created_at,
                        prompt.updated_at
                    ],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
        }
        transaction
            .commit()
            .map_err(|e| AppError::Database(e.to_string()))
    }

    /// 删除提示词
    pub fn delete_prompt(&self, app_type: &str, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM prompts WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(id: &str, name: &str, created_at: i64) -> Prompt {
        Prompt {
            id: id.to_string(),
            name: name.to_string(),
            content: format!("content of {id}"),
            description: Some(format!("about {id}")),
            enabled: true,
            created_at: Some(created_at),
            updated_at: Some(created_at + 1),
        }
    }

    fn ids(prompts: &IndexMap<String, Prompt>) -> Vec<String> {
        prompts.keys().cloned().collect()
    }

    #[test]
    fn prompts_round_trip_in_created_order_within_one_app() {
        let db = Database::memory().expect("memory db");
        db.save_prompt("claude", &prompt("second", "Second", 200))
            .unwrap();
        db.save_prompt("claude", &prompt("first", "First", 100))
            .unwrap();
        // 同一时间戳按 id 稳定排序。
        db.save_prompt("claude", &prompt("zero", "Zero", 100))
            .unwrap();
        // 别的 app 的同 id 行不能串门。
        db.save_prompt("codex", &prompt("first", "First", 100))
            .unwrap();

        let claude = db.get_prompts("claude").unwrap();
        assert_eq!(ids(&claude), vec!["first", "zero", "second"]);
        let stored = &claude["first"];
        assert_eq!(stored.name, "First");
        assert_eq!(stored.content, "content of first");
        assert_eq!(stored.description.as_deref(), Some("about first"));
        assert!(stored.enabled);
        assert_eq!(stored.created_at, Some(100));
        assert_eq!(stored.updated_at, Some(101));

        let codex = db.get_prompts("codex").unwrap();
        assert_eq!(ids(&codex), vec!["first"]);
        assert_eq!(db.get_prompts("gemini").unwrap().len(), 0);
    }

    #[test]
    fn save_prompt_replaces_by_id_and_delete_is_scoped_to_its_app() {
        let db = Database::memory().expect("memory db");
        db.save_prompt("claude", &prompt("shared", "Claude copy", 100))
            .unwrap();
        db.save_prompt("codex", &prompt("shared", "Codex copy", 100))
            .unwrap();

        // REPLACE：同 id 覆盖而不新增。
        let mut edited = prompt("shared", "Claude edited", 100);
        edited.enabled = false;
        edited.description = None;
        db.save_prompt("claude", &edited).unwrap();

        let claude = db.get_prompts("claude").unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude["shared"].name, "Claude edited");
        assert!(!claude["shared"].enabled);
        assert_eq!(claude["shared"].description, None);
        // codex 的同 id 行不受影响。
        assert_eq!(
            db.get_prompts("codex").unwrap()["shared"].name,
            "Codex copy"
        );

        // 删错 app 是空操作。
        db.delete_prompt("gemini", "shared").unwrap();
        assert_eq!(db.get_prompts("claude").unwrap().len(), 1);
        assert_eq!(db.get_prompts("codex").unwrap().len(), 1);

        db.delete_prompt("claude", "shared").unwrap();
        assert_eq!(db.get_prompts("claude").unwrap().len(), 0);
        assert_eq!(db.get_prompts("codex").unwrap().len(), 1);
    }

    #[test]
    fn mcode_batch_save_writes_every_row_and_leaves_other_apps_alone() {
        let db = Database::memory().expect("memory db");
        db.save_prompt("claude", &prompt("keep", "Keep", 100))
            .unwrap();

        let batch = IndexMap::from([
            ("one".to_string(), prompt("one", "One", 100)),
            ("two".to_string(), prompt("two", "Two", 200)),
        ]);
        db.save_mcode_prompts(&batch).unwrap();
        assert_eq!(ids(&db.get_prompts("mcode").unwrap()), vec!["one", "two"]);

        // 批量重写只碰 mcode 行，未提及的历史行原样留着。
        let mut rewritten = prompt("one", "One rewritten", 300);
        rewritten.enabled = false;
        let rerun = IndexMap::from([
            ("one".to_string(), rewritten),
            ("three".to_string(), prompt("three", "Three", 300)),
        ]);
        db.save_mcode_prompts(&rerun).unwrap();

        let mcode = db.get_prompts("mcode").unwrap();
        assert_eq!(ids(&mcode), vec!["two", "one", "three"]);
        assert_eq!(mcode["one"].name, "One rewritten");
        assert!(!mcode["one"].enabled);
        assert_eq!(mcode["one"].updated_at, Some(301));
        assert_eq!(mcode["two"].name, "Two");
        assert_eq!(db.get_prompts("claude").unwrap().len(), 1);
    }
}
