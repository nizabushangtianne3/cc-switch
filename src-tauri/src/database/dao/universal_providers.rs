//! 统一供应商 (Universal Provider) DAO
//!
//! 提供统一供应商的 CRUD 操作。

use crate::database::{lock_conn, to_json_string, Database};
use crate::error::AppError;
use crate::provider::UniversalProvider;
use std::collections::HashMap;

/// 统一供应商的 Settings Key
const UNIVERSAL_PROVIDERS_KEY: &str = "universal_providers";

impl Database {
    /// 获取所有统一供应商
    pub fn get_all_universal_providers(
        &self,
    ) -> Result<HashMap<String, UniversalProvider>, AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn
            .prepare("SELECT value FROM settings WHERE key = ?")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let result: Option<String> = stmt
            .query_row([UNIVERSAL_PROVIDERS_KEY], |row| row.get(0))
            .ok();

        match result {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("解析统一供应商数据失败: {e}"))),
            None => Ok(HashMap::new()),
        }
    }

    /// 获取单个统一供应商
    pub fn get_universal_provider(&self, id: &str) -> Result<Option<UniversalProvider>, AppError> {
        let providers = self.get_all_universal_providers()?;
        Ok(providers.get(id).cloned())
    }

    /// 保存统一供应商（添加或更新）
    pub fn save_universal_provider(&self, provider: &UniversalProvider) -> Result<(), AppError> {
        let mut providers = self.get_all_universal_providers()?;
        providers.insert(provider.id.clone(), provider.clone());
        self.save_all_universal_providers(&providers)
    }

    /// 删除统一供应商
    pub fn delete_universal_provider(&self, id: &str) -> Result<bool, AppError> {
        let mut providers = self.get_all_universal_providers()?;
        let existed = providers.remove(id).is_some();
        if existed {
            self.save_all_universal_providers(&providers)?;
        }
        Ok(existed)
    }

    /// 保存所有统一供应商（内部方法）
    fn save_all_universal_providers(
        &self,
        providers: &HashMap<String, UniversalProvider>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let json = to_json_string(providers)?;

        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)",
            [UNIVERSAL_PROVIDERS_KEY, &json],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{UniversalProvider, UniversalProviderApps};

    fn provider(id: &str, name: &str) -> UniversalProvider {
        UniversalProvider {
            id: id.to_string(),
            name: name.to_string(),
            provider_type: "newapi".to_string(),
            apps: UniversalProviderApps {
                claude: true,
                codex: false,
                gemini: false,
            },
            base_url: "https://api.example.com".to_string(),
            api_key: "sk-test".to_string(),
            models: Default::default(),
            website_url: None,
            notes: None,
            icon: None,
            icon_color: None,
            meta: None,
            created_at: None,
            sort_index: None,
        }
    }

    #[test]
    fn the_store_round_trips_universal_providers() {
        let db = Database::memory().expect("memory db");
        assert!(db.get_all_universal_providers().unwrap().is_empty());

        db.save_universal_provider(&provider("u-1", "One"))
            .expect("save the provider");
        let saved = db
            .get_universal_provider("u-1")
            .expect("read it back")
            .expect("the provider is there");
        assert_eq!(saved.name, "One");
        assert_eq!(saved.provider_type, "newapi");
        assert!(saved.apps.claude);

        // 同一个 id 再存是覆盖，不是追加。
        db.save_universal_provider(&provider("u-1", "Renamed"))
            .expect("overwrite the provider");
        let all = db.get_all_universal_providers().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["u-1"].name, "Renamed");
    }

    #[test]
    fn delete_reports_whether_the_provider_was_there() {
        let db = Database::memory().expect("memory db");
        db.save_universal_provider(&provider("u-1", "One"))
            .expect("save the provider");

        assert!(
            !db.delete_universal_provider("ghost").unwrap(),
            "没有这个号不算删掉"
        );
        assert!(db.delete_universal_provider("u-1").unwrap());
        assert!(db.get_all_universal_providers().unwrap().is_empty());
    }

    #[test]
    fn corrupt_json_surfaces_as_an_error() {
        let db = Database::memory().expect("memory db");
        db.set_setting(UNIVERSAL_PROVIDERS_KEY, "not json")
            .expect("seed the broken blob");
        assert!(
            db.get_all_universal_providers().is_err(),
            "坏数据要报错，不能静默按空表处理"
        );
    }
}
