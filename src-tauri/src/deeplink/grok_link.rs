//! Grok Build 供应商链接附带的 `config.toml`：只收选中的那张 `[model."<名称>"]` 表。
//!
//! 一个供应商在 live 里只拥有一张模型表（`live/project/grok.rs`），所以链接里的其他表不
//! 导入。选中的表按白名单收：显示名、说明、接口类型、窗口、采样和重试参数等照搬；地址、
//! Key、模型名用合并后的值（URL 参数优先）。
//!
//! 链接是不可信输入：让 Grok 运行凭据提供器（`auth_provider`）、读环境变量
//! （`env_http_headers`）或本机证书（`mtls_cert_dir`）的表直接拒绝；`env_key` 丢掉，Key
//! 用链接自己给的（只给了 `env_key` 的链接由调用方拒绝）。

use toml_edit::{DocumentMut, Item, Table, TableLike};

use super::utils::{non_empty_str, undecorated};
use crate::error::AppError;
use crate::grok_config::{DEFAULT_API_BACKEND, DEFAULT_CONTEXT_WINDOW};

/// 可以从链接带进来的键（Grok Build 1.0.50 的模型表，去掉地址、凭据和下面拒绝的）。
const PROFILE_KEYS: &[&str] = &[
    "name",
    "description",
    "notice",
    "model_family",
    "api_backend",
    "auth_scheme",
    "extra_headers",
    "query_params",
    "context_window",
    "context_windows",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "max_request_bytes",
    "auto_compact_threshold_percent",
    "system_prompt_label",
    "use_concise",
    "agent_type",
    "inference_idle_timeout_secs",
    "max_retries",
    "rate_limit_retry_threshold",
    "subagent_rate_limit_max_attempts",
    "hidden",
    "supports_reasoning_effort",
    "reasoning_efforts",
    "supports_backend_search",
    "compaction_at_tokens",
    "show_model_fingerprint",
    "stream_tool_calls",
    "reasoning_summary",
];

/// 运行凭据提供器、读环境变量或本机证书：链接带了就拒绝导入。
const REJECTED_PROFILE_KEYS: &[&str] = &["auth_provider", "env_http_headers", "mtls_cert_dir"];

/// 链接里选中的模型表，已按白名单过滤。
pub(super) struct GrokLinkProfile {
    /// 表名（`models.default` 指向的名字）。
    pub name: String,
    table: Table,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    /// 表用 `env_key` 取 Key。
    pub declares_env_key: bool,
}

impl GrokLinkProfile {
    pub(super) fn parse(text: &str) -> Result<Self, AppError> {
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| AppError::InvalidInput(format!("Invalid Grok Build config.toml: {e}")))?;
        let (name, source) = selected_profile(&doc)?;
        if let Some(key) = REJECTED_PROFILE_KEYS
            .iter()
            .find(|key| source.get(key).is_some())
        {
            return Err(AppError::InvalidInput(format!(
                "The Grok Build config uses '{key}', which makes Grok run a credential provider or \
                 read credentials from this computer; it cannot be imported from a link. Add the \
                 provider manually instead."
            )));
        }

        let mut table = Table::new();
        for key in PROFILE_KEYS {
            if let Some(item) = source.get(key) {
                table.insert(key, undecorated(item));
            }
        }
        Ok(Self {
            name,
            table,
            model: non_empty_str(source.get("model")),
            base_url: non_empty_str(source.get("base_url")),
            api_key: non_empty_str(source.get("api_key")),
            declares_env_key: source.get("env_key").is_some(),
        })
    }

    pub(super) fn model(&self) -> Option<String> {
        self.model.clone()
    }

    pub(super) fn base_url(&self) -> Option<String> {
        self.base_url.clone()
    }

    pub(super) fn api_key(&self) -> Option<String> {
        self.api_key.clone()
    }

    /// 写成供应商行的 `config`。地址、Key、模型名用合并后的值；表里没写显示名、接口类型、
    /// 窗口时补上和模板一样的默认值（live 的形状校验要求它们）。
    pub(super) fn render(
        &self,
        display_name: &str,
        model: &str,
        endpoint: &str,
        api_key: &str,
    ) -> String {
        let mut table = Table::new();
        table.insert("model", toml_edit::value(model));
        table.insert("base_url", toml_edit::value(endpoint));
        table.insert(
            "name",
            self.table
                .get("name")
                .filter(|item| non_empty_str(Some(item)).is_some())
                .cloned()
                .unwrap_or_else(|| toml_edit::value(display_name)),
        );
        table.insert("api_key", toml_edit::value(api_key));
        table.insert(
            "api_backend",
            self.table
                .get("api_backend")
                .cloned()
                .unwrap_or_else(|| toml_edit::value(DEFAULT_API_BACKEND)),
        );
        table.insert(
            "context_window",
            self.table
                .get("context_window")
                .cloned()
                .unwrap_or_else(|| toml_edit::value(DEFAULT_CONTEXT_WINDOW)),
        );
        for (key, item) in self.table.iter() {
            if !table.contains_key(key) {
                table.insert(key, item.clone());
            }
        }

        let mut doc = DocumentMut::new();
        let mut models = Table::new();
        models.insert("default", toml_edit::value(&self.name));
        doc.insert("models", Item::Table(models));
        let mut model_tables = Table::new();
        model_tables.set_implicit(true);
        model_tables.insert(&self.name, Item::Table(table));
        doc.insert("model", Item::Table(model_tables));
        doc.to_string()
    }
}

/// `models.default` 指向的表；没写或指不到时取唯一的一张表。
fn selected_profile(doc: &DocumentMut) -> Result<(String, &dyn TableLike), AppError> {
    let tables = doc
        .get("model")
        .and_then(Item::as_table_like)
        .ok_or_else(|| {
            AppError::InvalidInput(
                "The Grok Build config has no [model.\"<name>\"] table to import".to_string(),
            )
        })?;
    let default = non_empty_str(doc.get("models").and_then(|models| models.get("default")));
    if let Some(name) = default {
        if let Some(table) = tables.get(&name).and_then(Item::as_table_like) {
            return Ok((name, table));
        }
    }
    let mut candidates = tables
        .iter()
        .filter_map(|(name, item)| Some((name.to_string(), item.as_table_like()?)));
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(AppError::InvalidInput(
            "The Grok Build config has no [model.\"<name>\"] table to import".to_string(),
        )),
        _ => Err(AppError::InvalidInput(
            "The Grok Build config has several [model.*] tables; set [models] default to the one \
             to import"
                .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_error(text: &str) -> String {
        match GrokLinkProfile::parse(text) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected the Grok link config to be refused"),
        }
    }

    #[test]
    fn parse_keeps_whitelisted_keys_and_drops_the_rest() {
        let text = r#"
[models]
default = "relay"

[model.relay]
name = "Relay"
description = "A relay"
temperature = 0.5
note_from_the_link = "should be dropped"
"#;
        let profile = GrokLinkProfile::parse(text).unwrap();
        assert_eq!(profile.name, "relay");
        assert!(profile.table.get("name").is_some());
        assert!(profile.table.get("temperature").is_some());
        assert!(profile.table.get("note_from_the_link").is_none());
        assert!(!profile.declares_env_key);
    }

    #[test]
    fn parse_refuses_keys_that_make_grok_read_local_credentials() {
        for key in REJECTED_PROFILE_KEYS {
            let text = format!("[model.relay]\nname = \"Relay\"\n{key} = {{}}\n");
            let message = parse_error(&text);
            assert!(
                message.contains(key),
                "unexpected message for {key}: {message}"
            );
        }
    }

    #[test]
    fn parse_flags_env_key_and_drops_it_from_the_table() {
        let text = r#"
[model.relay]
name = "Relay"
env_key = "MY_KEY"
"#;
        let profile = GrokLinkProfile::parse(text).unwrap();
        assert!(profile.declares_env_key);
        assert!(profile.table.get("env_key").is_none());
    }

    #[test]
    fn parse_reads_model_endpoint_and_key_from_the_link() {
        let text = r#"
[model.relay]
name = "Relay"
model = "grok-4"
base_url = "https://relay.example.com/v1"
api_key = "sk-from-table"
"#;
        let profile = GrokLinkProfile::parse(text).unwrap();
        assert_eq!(profile.model().as_deref(), Some("grok-4"));
        assert_eq!(
            profile.base_url().as_deref(),
            Some("https://relay.example.com/v1")
        );
        assert_eq!(profile.api_key().as_deref(), Some("sk-from-table"));
    }

    #[test]
    fn parse_refuses_a_config_without_any_model_table() {
        assert!(parse_error("model_provider = \"custom\"").contains("no [model."));
    }

    #[test]
    fn parse_refuses_several_tables_without_a_default() {
        let text = r#"
[model.a]
name = "A"

[model.b]
name = "B"
"#;
        let message = parse_error(text);
        assert!(message.contains("several [model.*] tables"), "{message}");
    }

    #[test]
    fn parse_rejects_broken_toml() {
        assert!(parse_error("not = = toml").contains("Invalid Grok Build config.toml"));
    }

    #[test]
    fn render_writes_the_route_row_with_link_values_and_defaults() {
        let text = r#"
[model.relay]
model = "grok-4"
base_url = "https://old.example.com/v1"
api_key = "sk-stale"
"#;
        let profile = GrokLinkProfile::parse(text).unwrap();
        let rendered = profile.render(
            "Display Name",
            "grok-5",
            "https://relay.example.com/v1",
            "sk-link",
        );
        let doc = rendered.parse::<DocumentMut>().unwrap();

        assert_eq!(doc["models"]["default"].as_str(), Some("relay"));
        let table = doc["model"]["relay"].as_table_like().unwrap();
        // 链接参数盖过表里的旧值。
        assert_eq!(non_empty_str(table.get("model")).as_deref(), Some("grok-5"));
        assert_eq!(
            non_empty_str(table.get("base_url")).as_deref(),
            Some("https://relay.example.com/v1")
        );
        assert_eq!(
            non_empty_str(table.get("api_key")).as_deref(),
            Some("sk-link")
        );
        // 显示名缺省用链接展示名，接口和窗口缺省用模板默认值。
        assert_eq!(
            non_empty_str(table.get("name")).as_deref(),
            Some("Display Name")
        );
        assert_eq!(
            non_empty_str(table.get("api_backend")).as_deref(),
            Some(DEFAULT_API_BACKEND)
        );
        assert_eq!(
            table.get("context_window").and_then(Item::as_integer),
            Some(DEFAULT_CONTEXT_WINDOW)
        );
    }

    #[test]
    fn render_keeps_the_table_values_when_the_link_does_not_override_them() {
        let text = r#"
[model.relay]
name = "Relay"
api_backend = "chat_completions"
context_window = 777
"#;
        let profile = GrokLinkProfile::parse(text).unwrap();
        let rendered = profile.render(
            "Display Name",
            "grok-5",
            "https://relay.example.com/v1",
            "sk-link",
        );
        let doc = rendered.parse::<DocumentMut>().unwrap();
        let table = doc["model"]["relay"].as_table_like().unwrap();
        assert_eq!(non_empty_str(table.get("name")).as_deref(), Some("Relay"));
        assert_eq!(
            non_empty_str(table.get("api_backend")).as_deref(),
            Some("chat_completions")
        );
        assert_eq!(
            table.get("context_window").and_then(Item::as_integer),
            Some(777)
        );
    }
}
