//! JSON → SQLite 数据迁移
//!
//! 将旧版 config.json (MultiAppConfig) 数据迁移到 SQLite 数据库。

use super::{lock_conn, to_json_string, Database};
use crate::app_config::MultiAppConfig;
use crate::error::AppError;
use rusqlite::{params, Connection};

impl Database {
    /// 从 MultiAppConfig 迁移数据到数据库
    pub fn migrate_from_json(&self, config: &MultiAppConfig) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Self::migrate_from_json_tx(&tx, config)?;

        tx.commit()
            .map_err(|e| AppError::Database(format!("Commit migration failed: {e}")))?;
        Ok(())
    }

    /// 运行迁移的 dry-run 模式（在内存数据库中验证，不写入磁盘）
    ///
    /// 用于部署前验证迁移逻辑是否正确。
    pub fn migrate_from_json_dry_run(config: &MultiAppConfig) -> Result<(), AppError> {
        let mut conn =
            Connection::open_in_memory().map_err(|e| AppError::Database(e.to_string()))?;
        Self::create_tables_on_conn(&conn)?;
        Self::apply_schema_migrations_on_conn(&conn)?;

        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        Self::migrate_from_json_tx(&tx, config)?;

        // 显式 drop transaction 而不提交（内存数据库会被丢弃）
        drop(tx);
        Ok(())
    }

    /// 在事务中执行迁移
    fn migrate_from_json_tx(
        tx: &rusqlite::Transaction<'_>,
        config: &MultiAppConfig,
    ) -> Result<(), AppError> {
        // 1. 迁移 Providers
        Self::migrate_providers(tx, config)?;

        // 2. 迁移 MCP Servers
        Self::migrate_mcp_servers(tx, config)?;

        // 3. 迁移 Prompts
        Self::migrate_prompts(tx, config)?;

        // 4. 迁移 Skills
        Self::migrate_skills(tx, config)?;

        // 5. 迁移 Common Config
        Self::migrate_common_config(tx, config)?;

        Ok(())
    }

    /// 迁移供应商数据
    fn migrate_providers(
        tx: &rusqlite::Transaction<'_>,
        config: &MultiAppConfig,
    ) -> Result<(), AppError> {
        for (app_key, manager) in &config.apps {
            let app_type = app_key;
            let current_id = &manager.current;

            for (id, provider) in &manager.providers {
                let is_current = if id == current_id { 1 } else { 0 };

                // 处理 meta 和 endpoints
                let mut meta_clone = provider.meta.clone().unwrap_or_default();
                let endpoints = std::mem::take(&mut meta_clone.custom_endpoints);

                tx.execute(
                    "INSERT OR REPLACE INTO providers (
                        id, app_type, name, settings_config, website_url, category,
                        created_at, sort_index, notes, icon, icon_color, meta, is_current
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        id,
                        app_type,
                        provider.name,
                        to_json_string(&provider.settings_config)?,
                        provider.website_url,
                        provider.category,
                        provider.created_at,
                        provider.sort_index,
                        provider.notes,
                        provider.icon,
                        provider.icon_color,
                        to_json_string(&meta_clone)?,
                        is_current,
                    ],
                )
                .map_err(|e| AppError::Database(format!("Migrate provider failed: {e}")))?;

                // 迁移 Endpoints
                for (url, endpoint) in endpoints {
                    tx.execute(
                        "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, app_type, url, endpoint.added_at],
                    )
                    .map_err(|e| AppError::Database(format!("Migrate endpoint failed: {e}")))?;
                }
            }
        }
        Ok(())
    }

    /// 迁移 MCP 服务器数据
    fn migrate_mcp_servers(
        tx: &rusqlite::Transaction<'_>,
        config: &MultiAppConfig,
    ) -> Result<(), AppError> {
        if let Some(servers) = &config.mcp.servers {
            for (id, server) in servers {
                tx.execute(
                    "INSERT OR REPLACE INTO mcp_servers (
                        id, name, server_config, description, homepage, docs, tags,
                        enabled_claude, enabled_codex, enabled_gemini
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id,
                        server.name,
                        to_json_string(&server.server)?,
                        server.description,
                        server.homepage,
                        server.docs,
                        to_json_string(&server.tags)?,
                        server.apps.claude,
                        server.apps.codex,
                        server.apps.gemini,
                    ],
                )
                .map_err(|e| AppError::Database(format!("Migrate mcp server failed: {e}")))?;
            }
        }
        Ok(())
    }

    /// 迁移提示词数据
    fn migrate_prompts(
        tx: &rusqlite::Transaction<'_>,
        config: &MultiAppConfig,
    ) -> Result<(), AppError> {
        let migrate_app_prompts = |prompts_map: &std::collections::HashMap<
            String,
            crate::prompt::Prompt,
        >,
                                   app_type: &str|
         -> Result<(), AppError> {
            for (id, prompt) in prompts_map {
                tx.execute(
                        "INSERT OR REPLACE INTO prompts (
                            id, app_type, name, content, description, enabled, created_at, updated_at
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            id,
                            app_type,
                            prompt.name,
                            prompt.content,
                            prompt.description,
                            prompt.enabled,
                            prompt.created_at,
                            prompt.updated_at,
                        ],
                    )
                    .map_err(|e| AppError::Database(format!("Migrate prompt failed: {e}")))?;
            }
            Ok(())
        };

        migrate_app_prompts(&config.prompts.claude.prompts, "claude")?;
        migrate_app_prompts(&config.prompts.codex.prompts, "codex")?;
        migrate_app_prompts(&config.prompts.gemini.prompts, "gemini")?;

        Ok(())
    }

    /// 迁移 Skills 数据
    fn migrate_skills(
        tx: &rusqlite::Transaction<'_>,
        config: &MultiAppConfig,
    ) -> Result<(), AppError> {
        // v3.10.0+：Skills 的 SSOT 已迁移到文件系统（~/.cc-switch/skills/）+ 数据库统一结构。
        //
        // 旧版 config.json 里的 `skills.skills` 仅记录“安装状态”，但不包含完整元数据，
        // 且无法保证 SSOT 目录中一定存在对应的 skill 文件。
        //
        // 因此这里不再直接把旧的安装状态写入新 skills 表，避免产生“数据库显示已安装但文件缺失”的不一致。
        // 迁移后可通过：
        // - 前端「导入已有」(扫描各应用的 skills 目录并复制到 SSOT)
        // - 或后续启动时的自动扫描逻辑
        // 来重建已安装技能记录。

        for repo in &config.skills.repos {
            tx.execute(
                "INSERT OR REPLACE INTO skill_repos (owner, name, branch, enabled) VALUES (?1, ?2, ?3, ?4)",
                params![repo.owner, repo.name, repo.branch, repo.enabled],
            ).map_err(|e| AppError::Database(format!("Migrate skill repo failed: {e}")))?;
        }

        Ok(())
    }

    /// 迁移通用配置片段
    fn migrate_common_config(
        tx: &rusqlite::Transaction<'_>,
        config: &MultiAppConfig,
    ) -> Result<(), AppError> {
        if let Some(snippet) = &config.common_config_snippets.claude {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params!["common_config_claude", snippet],
            )
            .map_err(|e| AppError::Database(format!("Migrate settings failed: {e}")))?;
        }
        if let Some(snippet) = &config.common_config_snippets.codex {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params!["common_config_codex", snippet],
            )
            .map_err(|e| AppError::Database(format!("Migrate settings failed: {e}")))?;
        }
        if let Some(snippet) = &config.common_config_snippets.gemini {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params!["common_config_gemini", snippet],
            )
            .map_err(|e| AppError::Database(format!("Migrate settings failed: {e}")))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::{McpApps, McpServer, MultiAppConfig, PromptConfig};
    use crate::prompt::Prompt;
    use crate::provider::{Provider, ProviderManager};
    use crate::services::skill::SkillRepo;
    use crate::settings::CustomEndpoint;
    use indexmap::IndexMap;
    use serde_json::json;

    fn provider_row(id: &str) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            format!("{id} name"),
            json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-test"}}),
            Some("https://example.com".to_string()),
        );
        provider.category = Some("official".to_string());
        provider.created_at = Some(1_700_000_000);
        provider.sort_index = Some(3);
        provider.notes = Some("note".to_string());
        provider.icon = Some("openai".to_string());
        provider.icon_color = Some("#00A67E".to_string());
        let meta = crate::provider::ProviderMeta {
            custom_endpoints: std::collections::HashMap::from([(
                "https://api.example.com/v1".to_string(),
                CustomEndpoint {
                    url: "https://api.example.com/v1".to_string(),
                    added_at: 1_700_000_001,
                    last_used: None,
                },
            )]),
            ..Default::default()
        };
        provider.meta = Some(meta);
        provider
    }

    fn one_provider_config(app_type: &str, id: &str) -> MultiAppConfig {
        let mut config = MultiAppConfig::default();
        config.apps.insert(
            app_type.to_string(),
            ProviderManager {
                providers: IndexMap::from([(id.to_string(), provider_row(id))]),
                current: id.to_string(),
            },
        );
        config
    }

    #[test]
    fn migration_moves_providers_their_endpoints_and_the_current_pointer() {
        let db = Database::memory().expect("memory db");
        db.migrate_from_json(&one_provider_config("claude", "p1"))
            .unwrap();
        db.migrate_from_json(&one_provider_config("codex", "p2"))
            .unwrap();

        let claude = db.get_all_providers("claude").unwrap();
        assert_eq!(claude.len(), 1);
        let stored = &claude["p1"];
        assert_eq!(stored.name, "p1 name");
        assert_eq!(
            stored.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
            "sk-test"
        );
        assert_eq!(stored.category.as_deref(), Some("official"));
        assert_eq!(stored.created_at, Some(1_700_000_000));
        assert_eq!(stored.sort_index, Some(3));
        assert_eq!(stored.notes.as_deref(), Some("note"));
        assert_eq!(stored.icon.as_deref(), Some("openai"));
        assert_eq!(stored.icon_color.as_deref(), Some("#00A67E"));
        // 当前指针跟着搬过去，别的 app 不串门。
        assert_eq!(
            db.get_current_provider("claude").unwrap().as_deref(),
            Some("p1")
        );
        assert_eq!(db.get_all_providers("codex").unwrap().len(), 1);
        assert!(db.get_all_providers("gemini").unwrap().is_empty());

        // 自定义端点落到自己的表：落库的 meta JSON 里不带一份。
        let endpoints = db.get_custom_endpoints_by_provider("claude", "p1").unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].url, "https://api.example.com/v1");
        assert_eq!(endpoints[0].added_at, 1_700_000_001);
        {
            let conn = db.conn.lock().unwrap();
            let raw: String = conn
                .query_row(
                    "SELECT meta FROM providers WHERE id = 'p1' AND app_type = 'claude'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!raw.contains("api.example.com"), "{raw}");
        }
        let meta = stored.meta.as_ref().expect("meta round trip");
        // 读回时再把端点合进 meta（向前兼容的展示视图）。
        assert_eq!(meta.custom_endpoints.len(), 1);
        let restored = &meta.custom_endpoints["https://api.example.com/v1"];
        assert_eq!(restored.added_at, 1_700_000_001);
        assert_eq!(restored.last_used, None);
    }

    #[test]
    fn migration_moves_mcp_servers_prompts_repos_and_snippets() {
        let db = Database::memory().expect("memory db");
        let mut config = MultiAppConfig::default();

        config.mcp.servers = Some(std::collections::HashMap::from([(
            "m1".to_string(),
            McpServer {
                id: "m1".to_string(),
                name: "MCP One".to_string(),
                server: json!({"type": "stdio", "command": "node"}),
                apps: McpApps {
                    claude: true,
                    codex: true,
                    ..Default::default()
                },
                description: Some("desc".to_string()),
                homepage: Some("https://example.com".to_string()),
                docs: Some("https://docs.example.com".to_string()),
                tags: vec!["x".to_string()],
            },
        )]));
        config.prompts.claude = PromptConfig {
            prompts: std::collections::HashMap::from([(
                "pr1".to_string(),
                Prompt {
                    id: "pr1".to_string(),
                    name: "Prompt One".to_string(),
                    content: "content".to_string(),
                    description: Some("about".to_string()),
                    enabled: true,
                    created_at: Some(10),
                    updated_at: Some(11),
                },
            )]),
        };
        config.skills.repos = vec![SkillRepo {
            owner: "acme".to_string(),
            name: "skills".to_string(),
            branch: "main".to_string(),
            enabled: true,
        }];
        config.common_config_snippets.claude = Some("env A=1".to_string());
        config.common_config_snippets.codex = Some("model = \"x\"".to_string());

        db.migrate_from_json(&config).unwrap();

        let servers = db.get_all_mcp_servers().unwrap();
        let stored = &servers["m1"];
        assert_eq!(stored.name, "MCP One");
        assert_eq!(stored.server["command"], "node");
        assert!(stored.apps.claude);
        assert!(stored.apps.codex);
        assert!(!stored.apps.gemini);
        assert_eq!(stored.description.as_deref(), Some("desc"));
        assert_eq!(stored.homepage.as_deref(), Some("https://example.com"));
        assert_eq!(stored.docs.as_deref(), Some("https://docs.example.com"));
        assert_eq!(stored.tags, vec!["x".to_string()]);

        let prompts = db.get_prompts("claude").unwrap();
        assert_eq!(prompts["pr1"].name, "Prompt One");
        assert_eq!(prompts["pr1"].content, "content");
        assert!(prompts["pr1"].enabled);
        assert_eq!(prompts["pr1"].created_at, Some(10));
        assert_eq!(prompts["pr1"].updated_at, Some(11));

        let repos = db.get_skill_repos().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].owner, "acme");
        assert_eq!(repos[0].name, "skills");
        assert_eq!(repos[0].branch, "main");
        assert!(repos[0].enabled);

        assert_eq!(
            db.get_config_snippet("claude").unwrap().as_deref(),
            Some("env A=1")
        );
        assert_eq!(
            db.get_config_snippet("codex").unwrap().as_deref(),
            Some("model = \"x\"")
        );
        // 没写的片段不迁。
        assert_eq!(db.get_config_snippet("gemini").unwrap(), None);
    }

    #[test]
    fn dry_run_checks_the_payload_without_touching_a_database() {
        // dry-run 用一次性内存库跑同一套迁移：Ok 即校验通过，不写任何落库文件。
        Database::migrate_from_json_dry_run(&one_provider_config("claude", "p1")).unwrap();
        Database::migrate_from_json_dry_run(&MultiAppConfig::default()).unwrap();
    }

    #[test]
    fn migrating_twice_replaces_rows_instead_of_failing() {
        let db = Database::memory().expect("memory db");
        let config = one_provider_config("claude", "p1");
        db.migrate_from_json(&config).unwrap();
        db.migrate_from_json(&config).unwrap();

        let claude = db.get_all_providers("claude").unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(
            db.get_current_provider("claude").unwrap().as_deref(),
            Some("p1")
        );
    }
}
