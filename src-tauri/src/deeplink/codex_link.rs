//! Codex 供应商链接附带的 `config.toml`：只收属于这个供应商的部分。
//!
//! 链接是不可信输入。导入的行在切换时由投影写进 `~/.codex/config.toml`，降级后旧版还会
//! 把行里的 config 整份写进去，所以这里按白名单收：选路表里和连接有关的键，加上投影会写的
//! 顶层关键字段、独有字段和嵌套的模型名。用户自己的全局设置（`[features]`、`[tools]`、
//! MCP、sandbox、notify 等）不收，切换本来也不写它们。
//!
//! 让 Codex 从本机取凭据或执行命令的键直接拒绝，不能悄悄换成另一种鉴权方式；`env_key`
//! 丢掉，Key 用链接自己给的。

use toml_edit::{DocumentMut, Item, Table, TableLike, Value};
use url::Url;

use super::utils::{non_empty_str, undecorated, undecorated_value};
use crate::error::AppError;
use crate::live::floor;
use crate::live::project::codex::{ROUTE_ID, ROW_TOP_FIELDS};

/// 选路表里可以从链接带进来的键（Codex 0.162 的 `ModelProviderInfo`，去掉下面两类）。
const TABLE_KEYS: &[&str] = &[
    "name",
    "base_url",
    "wire_api",
    "requires_openai_auth",
    "experimental_bearer_token",
    "model_catalog_url",
    "capabilities",
    "query_params",
    "http_headers",
    "request_max_retries",
    "stream_max_retries",
    "stream_idle_timeout_ms",
    "websocket_connect_timeout_ms",
    "supports_websockets",
    "supports_standalone_web_search",
];

/// 让 Codex 执行命令（`auth`）、读本机的云凭据（`aws`、`gateway_oauth`）或环境变量
/// （`env_http_headers`）取凭据。链接带了就拒绝导入。
const REJECTED_TABLE_KEYS: &[&str] = &["auth", "aws", "gateway_oauth", "env_http_headers"];

/// 从环境变量取 Key：丢掉，改用链接给的 Key。链接只给了它、没给 Key 时由调用方报错。
const ENV_KEY_FIELDS: &[&str] = &["env_key", "env_key_instructions"];

/// 链接 config 参数里的 Codex 配置文本。
///
/// 两种写法：`{"auth": {...}, "config": "<toml 文本>"}`（和供应商行同形），或者整份就是
/// `config.toml`（`configFormat=toml`，或 JSON 写的同一份内容）。只带 `auth` 的返回 `None`。
pub(super) fn config_text(config: &serde_json::Value) -> Result<Option<String>, AppError> {
    if let Some(text) = config.get("config").and_then(serde_json::Value::as_str) {
        return Ok(Some(text.to_string()));
    }
    let Some(object) = config.as_object() else {
        return Ok(None);
    };
    if object.is_empty() || object.contains_key("auth") || object.contains_key("config") {
        return Ok(None);
    }
    let value: toml::Value = serde_json::from_value(config.clone())
        .map_err(|e| AppError::InvalidInput(format!("Invalid Codex config: {e}")))?;
    toml::to_string(&value)
        .map(Some)
        .map_err(|e| AppError::InvalidInput(format!("Invalid Codex config: {e}")))
}

/// 链接里的 Codex 配置，已按白名单过滤。
pub(super) struct CodexLinkConfig {
    /// 链接选中的供应商表。
    table: Option<Table>,
    /// 顶层的关键字段和独有字段，按投影的顺序。
    top: Vec<(&'static str, Value)>,
    /// 嵌在 `[agents]`、`[memories]` 里的模型名。
    nested: Vec<(&'static [&'static str], Value)>,
    /// 选中的表用 `env_key` 取 Key。
    pub declares_env_key: bool,
}

impl CodexLinkConfig {
    pub(super) fn parse(text: &str) -> Result<Self, AppError> {
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| AppError::InvalidInput(format!("Invalid Codex config.toml: {e}")))?;

        let mut declares_env_key = false;
        let table = match selected_table(&doc) {
            Some(source) => {
                if let Some(key) = REJECTED_TABLE_KEYS
                    .iter()
                    .find(|key| source.get(key).is_some())
                {
                    return Err(AppError::InvalidInput(format!(
                        "The Codex config uses '{key}', which makes Codex run a command or read \
                         credentials from this computer; it cannot be imported from a link. Add \
                         the provider manually instead."
                    )));
                }
                declares_env_key = ENV_KEY_FIELDS.iter().any(|key| source.get(key).is_some());
                let mut table = Table::new();
                for key in TABLE_KEYS {
                    if let Some(item) = source.get(key) {
                        table.insert(key, undecorated(item));
                    }
                }
                Some(table)
            }
            None => None,
        };

        let top = ROW_TOP_FIELDS
            .iter()
            .chain(floor::CODEX_EXCLUSIVE_TOP)
            .filter_map(|key| {
                let value = doc.get(key)?.as_value()?;
                Some((*key, undecorated_value(value)))
            })
            .collect();
        let nested = floor::CODEX_FLOOR_NESTED
            .iter()
            .filter_map(|segments| {
                let value = value_at(doc.as_table(), segments)?;
                Some((*segments, undecorated_value(value)))
            })
            .collect();

        Ok(Self {
            table,
            top,
            nested,
            declares_env_key,
        })
    }

    /// 选中的表里的地址。
    pub(super) fn base_url(&self) -> Option<String> {
        non_empty_str(self.table.as_ref()?.get("base_url"))
    }

    pub(super) fn model(&self) -> Option<String> {
        self.top
            .iter()
            .find(|(key, _)| *key == "model")
            .and_then(|(_, value)| value.as_str())
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
    }

    /// 写成供应商行的 `config`：一律选路到 `[model_providers.custom]`。链接参数优先：
    /// 地址、模型名、Key 用合并后的值（URL 参数已经盖过配置里的）。
    pub(super) fn render(
        &self,
        display_name: &str,
        endpoint: &str,
        model: &str,
        api_key: &str,
    ) -> Result<String, AppError> {
        let mut doc = DocumentMut::new();
        doc["model_provider"] = toml_edit::value(ROUTE_ID);
        doc["model"] = toml_edit::value(model);
        for (key, value) in self.top.iter().filter(|(key, _)| *key != "model") {
            doc[*key] = Item::Value(value.clone());
        }
        for (segments, value) in &self.nested {
            let (last, parents) = segments.split_last().expect("nested path is not empty");
            let mut current = doc.as_table_mut();
            for segment in parents {
                current = current
                    .entry(segment)
                    .or_insert_with(|| Item::Table(Table::new()))
                    .as_table_mut()
                    .expect("nested parent is a table");
            }
            current.insert(last, Item::Value(value.clone()));
        }

        let source = self.table.clone().unwrap_or_default();
        let mut table = Table::new();
        table.insert(
            "name",
            match non_empty_str(source.get("name")) {
                Some(name) => toml_edit::value(name),
                None => toml_edit::value(display_name),
            },
        );
        table.insert("base_url", toml_edit::value(endpoint));
        table.insert(
            "wire_api",
            source
                .get("wire_api")
                .cloned()
                .unwrap_or_else(|| toml_edit::value("responses")),
        );
        table.insert(
            "requires_openai_auth",
            source
                .get("requires_openai_auth")
                .cloned()
                .unwrap_or_else(|| toml_edit::value(true)),
        );
        for (key, item) in source.iter() {
            if table.contains_key(key) {
                continue;
            }
            if key == "experimental_bearer_token" {
                // 只留一份 Key：和 auth 里的同值，降级后旧版写进 live 也不会拿到过期的 Key。
                table.insert(key, toml_edit::value(api_key));
            } else {
                table.insert(key, item.clone());
            }
        }
        if let Some(catalog) = non_empty_str(table.get("model_catalog_url")) {
            check_catalog_origin(&catalog, endpoint)?;
        }

        let mut providers = Table::new();
        providers.set_implicit(true);
        providers.insert(ROUTE_ID, Item::Table(table));
        doc["model_providers"] = Item::Table(providers);
        Ok(doc.to_string())
    }
}

/// Codex 取模型目录时带着这个供应商的鉴权，目录地址必须和请求地址同源，不能借链接把 Key
/// 发到确认框里没显示的地方。
fn check_catalog_origin(catalog: &str, endpoint: &str) -> Result<(), AppError> {
    let same_origin = match (Url::parse(catalog), Url::parse(endpoint)) {
        (Ok(catalog), Ok(endpoint)) => catalog.origin() == endpoint.origin(),
        _ => false,
    };
    if same_origin {
        Ok(())
    } else {
        Err(AppError::InvalidInput(format!(
            "'model_catalog_url' must be on the same origin as the endpoint '{endpoint}', got \
             '{catalog}'"
        )))
    }
}

/// 链接选中的供应商表：`model_provider` 指向的那张，没写选路时取唯一的一张。
fn selected_table(doc: &DocumentMut) -> Option<&dyn TableLike> {
    let providers = doc.get("model_providers")?.as_table_like()?;
    match non_empty_str(doc.get("model_provider")) {
        Some(id) => providers.get(&id)?.as_table_like(),
        None if providers.len() == 1 => providers.iter().next()?.1.as_table_like(),
        None => None,
    }
}

fn value_at<'a>(root: &'a Table, segments: &[&str]) -> Option<&'a Value> {
    let (last, parents) = segments.split_last()?;
    let mut current: &dyn TableLike = root;
    for segment in parents {
        current = current.get(segment)?.as_table_like()?;
    }
    current.get(last)?.as_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: serde_json::Value) -> serde_json::Value {
        value
    }

    /// 解析失败的信息（`CodexLinkConfig` 没有 Debug，`unwrap_err` 用不了）。
    fn parse_error(text: &str) -> String {
        match CodexLinkConfig::parse(text) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected the Codex link config to be refused"),
        }
    }

    /// 渲染失败的信息（同上，`expect_err` 需要 `T: Debug`，这里给不了）。
    fn render_error(config: &CodexLinkConfig) -> String {
        match config.render(
            "Display Name",
            "https://api.example.com/v1",
            "gpt-5",
            "sk-link",
        ) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected the rendered config to be refused"),
        }
    }

    // ---------- config_text ----------

    #[test]
    fn config_text_reads_the_inline_config_string() {
        let config = json(serde_json::json!({"config": "model_provider = \"custom\""}));
        let text = config_text(&config).unwrap().expect("inline config");
        assert_eq!(text, "model_provider = \"custom\"");
    }

    #[test]
    fn config_text_refuses_an_auth_only_object() {
        let config = json(serde_json::json!({"auth": {"OPENAI_API_KEY": "sk-x"}}));
        assert_eq!(config_text(&config).unwrap(), None);
    }

    #[test]
    fn config_text_converts_a_whole_json_config_to_toml() {
        let config = json(serde_json::json!({
            "model": "gpt-5",
            "model_provider": "custom",
            "model_providers": {"custom": {"name": "Relay", "base_url": "https://api.example.com/v1"}}
        }));
        let text = config_text(&config).unwrap().expect("converted config");
        assert!(text.contains("model_providers"), "{text}");
        assert!(text.contains("[model_providers.custom]"), "{text}");
        assert!(!CodexLinkConfig::parse(&text).unwrap().declares_env_key);
    }

    #[test]
    fn config_text_returns_none_for_empty_or_non_object_values() {
        assert_eq!(config_text(&json(serde_json::json!({}))).unwrap(), None);
        assert_eq!(config_text(&json(serde_json::json!("text"))).unwrap(), None);
        // 带了 config 键但不是字符串：同样没有可用的配置文本。
        assert_eq!(
            config_text(&json(serde_json::json!({"config": 5}))).unwrap(),
            None
        );
    }

    // ---------- parse ----------

    #[test]
    fn parse_keeps_whitelisted_table_keys_and_drops_the_rest() {
        let text = r#"
model = "gpt-5"
model_provider = "custom"

[model_providers.custom]
name = "Relay"
base_url = "https://api.example.com/v1"
query_params = { api_version = "2024-05" }
note_from_the_link = "should be dropped"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let table = config.table.expect("selected table");
        assert_eq!(non_empty_str(table.get("name")).as_deref(), Some("Relay"));
        assert_eq!(
            non_empty_str(table.get("base_url")).as_deref(),
            Some("https://api.example.com/v1")
        );
        assert!(table.get("query_params").is_some());
        assert!(table.get("note_from_the_link").is_none());
    }

    #[test]
    fn parse_refuses_keys_that_make_codex_run_commands_or_read_local_credentials() {
        for key in REJECTED_TABLE_KEYS {
            let text = format!(
                "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"Relay\"\n{key} = {{}}\n"
            );
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
model_provider = "custom"

[model_providers.custom]
name = "Relay"
env_key = "MY_KEY"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        assert!(config.declares_env_key);
        let table = config.table.expect("selected table");
        assert!(table.get("env_key").is_none());
    }

    #[test]
    fn parse_selects_the_table_the_model_provider_points_at() {
        let text = r#"
model_provider = "second"

[model_providers.first]
name = "First"

[model_providers.second]
name = "Second"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let table = config.table.expect("selected table");
        assert_eq!(non_empty_str(table.get("name")).as_deref(), Some("Second"));
    }

    #[test]
    fn parse_falls_back_to_the_only_table_without_a_pointer() {
        let text = r#"
[model_providers.only]
name = "Only"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let table = config.table.expect("selected table");
        assert_eq!(non_empty_str(table.get("name")).as_deref(), Some("Only"));
    }

    #[test]
    fn parse_without_a_pointer_and_many_tables_selects_nothing() {
        let text = r#"
[model_providers.a]
name = "A"

[model_providers.b]
name = "B"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        assert!(config.table.is_none());
    }

    #[test]
    fn parse_reads_nested_model_names() {
        let text = r#"
model = "gpt-5"

[agents]
default_subagent_model = "gpt-5-mini"
default_subagent_reasoning_effort = "low"

[memories]
extract_model = "gpt-5-nano"
consolidation_model = "gpt-5"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let nested: Vec<(&[&str], String)> = config
            .nested
            .iter()
            .map(|(segments, value)| {
                (
                    *segments,
                    value
                        .as_str()
                        .expect("nested value is a string")
                        .to_string(),
                )
            })
            .collect();
        assert!(nested.contains(&(
            &["agents", "default_subagent_model"][..],
            "gpt-5-mini".to_string()
        )));
        assert!(nested.contains(&(&["memories", "extract_model"][..], "gpt-5-nano".to_string())));
    }

    #[test]
    fn parse_rejects_broken_toml() {
        let message = CodexLinkConfig::parse("not = = toml")
            .err()
            .expect("broken TOML must be refused")
            .to_string();
        assert!(message.contains("Invalid Codex config.toml"), "{message}");
    }

    #[test]
    fn base_url_and_model_read_the_link_values() {
        let text = r#"
model = "gpt-5"
model_provider = "custom"

[model_providers.custom]
name = "Relay"
base_url = "https://api.example.com/v1"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        assert_eq!(
            config.base_url().as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(config.model().as_deref(), Some("gpt-5"));
    }

    // ---------- render ----------

    #[test]
    fn render_writes_the_route_row_with_link_values() {
        let text = r#"
model = "ignored-model"
model_provider = "custom"

[model_providers.custom]
name = "Relay"
base_url = "https://old.example.com/v1"
wire_api = "responses"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let rendered = config
            .render(
                "Display Name",
                "https://api.example.com/v1",
                "gpt-5",
                "sk-link",
            )
            .unwrap();
        let doc = rendered.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some(ROUTE_ID));
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));
        let table = doc["model_providers"][ROUTE_ID].as_table_like().unwrap();
        assert_eq!(non_empty_str(table.get("name")).as_deref(), Some("Relay"));
        assert_eq!(
            non_empty_str(table.get("base_url")).as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(
            non_empty_str(table.get("wire_api")).as_deref(),
            Some("responses")
        );
    }

    #[test]
    fn render_falls_back_to_the_display_name_and_defaults() {
        let config = CodexLinkConfig::parse("model = \"gpt-5\"").unwrap();
        let rendered = config
            .render(
                "Display Name",
                "https://api.example.com/v1",
                "gpt-5",
                "sk-link",
            )
            .unwrap();
        let doc = rendered.parse::<DocumentMut>().unwrap();
        let table = doc["model_providers"][ROUTE_ID].as_table_like().unwrap();
        assert_eq!(
            non_empty_str(table.get("name")).as_deref(),
            Some("Display Name")
        );
        assert_eq!(
            non_empty_str(table.get("wire_api")).as_deref(),
            Some("responses")
        );
        assert_eq!(
            table
                .get("requires_openai_auth")
                .and_then(Item::as_value)
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn render_replaces_the_bearer_token_with_the_link_key() {
        let text = r#"
model_provider = "custom"

[model_providers.custom]
name = "Relay"
experimental_bearer_token = "stale-key-from-the-link"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let rendered = config
            .render(
                "Display Name",
                "https://api.example.com/v1",
                "gpt-5",
                "sk-link",
            )
            .unwrap();
        let doc = rendered.parse::<DocumentMut>().unwrap();
        let table = doc["model_providers"][ROUTE_ID].as_table_like().unwrap();
        assert_eq!(
            non_empty_str(table.get("experimental_bearer_token")).as_deref(),
            Some("sk-link")
        );
    }

    #[test]
    fn render_allows_a_catalog_url_on_the_endpoint_origin() {
        let text = r#"
model_provider = "custom"

[model_providers.custom]
name = "Relay"
model_catalog_url = "https://api.example.com/models.json"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let rendered = config
            .render(
                "Display Name",
                "https://api.example.com/v1",
                "gpt-5",
                "sk-link",
            )
            .expect("same-origin catalog");
        let doc = rendered.parse::<DocumentMut>().unwrap();
        let table = doc["model_providers"][ROUTE_ID].as_table_like().unwrap();
        assert_eq!(
            non_empty_str(table.get("model_catalog_url")).as_deref(),
            Some("https://api.example.com/models.json")
        );
    }

    #[test]
    fn render_rejects_a_catalog_url_on_another_origin() {
        let text = r#"
model_provider = "custom"

[model_providers.custom]
name = "Relay"
model_catalog_url = "https://evil.example.com/models.json"
"#;
        let config = CodexLinkConfig::parse(text).unwrap();
        let message = render_error(&config);
        assert!(message.contains("same origin"), "{message}");
        assert!(message.contains("evil.example.com"), "{message}");
    }
}
