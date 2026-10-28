use crate::config::atomic_write_private;
use crate::error::AppError;
use crate::store::AppState;
use serde_json::{json, Value};
use std::{path::Path, sync::Mutex};

static WRITE_LOCK: Mutex<()> = Mutex::new(());
const TRANSPORT_FIELDS: [&str; 6] = ["command", "args", "env", "url", "headers", "type"];

fn read(path: &Path) -> Result<Value, AppError> {
    // agy 首次启动会建一个 0 字节的 mcp_config.json，当成没有配置
    super::native::read_native(path, "Antigravity", true)
}

#[derive(Clone, Copy)]
pub(crate) enum AgyChange<'a> {
    Upsert(&'a Value),
    Disable,
    Remove,
}
impl<'a> AgyChange<'a> {
    pub(crate) fn from_enabled(spec: Option<&'a Value>) -> Self {
        spec.map_or(Self::Disable, Self::Upsert)
    }
}

pub fn sync(id: &str, spec: Option<&Value>) -> Result<(), AppError> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|e| AppError::Message(e.to_string()))?;
    sync_file(
        &crate::agy_config::customizations_dir().join("mcp_config.json"),
        id,
        AgyChange::from_enabled(spec),
    )
}

pub(crate) fn sync_and_commit<T>(
    id: &str,
    change: AgyChange<'_>,
    commit: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|e| AppError::Message(e.to_string()))?;
    let path = crate::agy_config::customizations_dir().join("mcp_config.json");
    crate::mcode_config::write_and_commit(&path, || sync_file(&path, id, change), commit)
}

fn sync_file(path: &Path, id: &str, change: AgyChange<'_>) -> Result<(), AppError> {
    let mut document = read(path)?;
    if document.get("mcpServers").is_none() {
        document["mcpServers"] = json!({});
    }
    let servers = document["mcpServers"].as_object_mut().unwrap();
    if let AgyChange::Upsert(spec) = change {
        super::validation::validate_server_spec(&unified_spec(spec))?;
        if spec["type"] == "sse" {
            return Err(AppError::Config(
                "Antigravity supports stdio and streamable HTTP, not SSE".into(),
            ));
        }
        let mut merged = servers.get(id).cloned().unwrap_or_else(|| json!({}));
        let object = merged
            .as_object_mut()
            .ok_or_else(|| AppError::Config("Invalid Antigravity MCP entry".into()))?;
        for field in TRANSPORT_FIELDS {
            object.remove(field);
        }
        object.extend(unified_spec(spec).as_object().unwrap().clone());
        object.remove("enabled");
        object.remove("disabled");
        object.remove("serverUrl");
        object.remove("type");
        object.insert("disabled".into(), json!(false));
        servers.insert(id.into(), merged);
    } else if matches!(change, AgyChange::Remove) {
        servers.remove(id);
    } else {
        if let Some(server) = servers.get_mut(id) {
            let object = server
                .as_object_mut()
                .ok_or_else(|| AppError::Config("Invalid Antigravity MCP entry".into()))?;
            object.insert("disabled".into(), json!(true));
        }
    }
    atomic_write_private(
        path,
        serde_json::to_string_pretty(&document)
            .map_err(|e| AppError::Config(e.to_string()))?
            .as_bytes(),
    )
}

pub fn import(state: &AppState) -> Result<usize, AppError> {
    super::native::import(
        state,
        &super::native::NativeSource {
            path: crate::agy_config::customizations_dir().join("mcp_config.json"),
            label: "Antigravity",
            tolerate_empty: true,
            enabled_field: "disabled",
            inverted: true,
            set_app: |apps, enabled| apps.agy = enabled,
            unify: unified_spec,
            transport_fields: &TRANSPORT_FIELDS,
        },
    )
}

fn unified_spec(native: &Value) -> Value {
    let mut spec = native.clone();
    if let Some(url) = spec.get("serverUrl").cloned() {
        if spec.get("url").is_none() {
            spec["url"] = url;
        }
        spec.as_object_mut().unwrap().remove("serverUrl");
    }
    if spec["type"] == "streamable-http"
        || (spec.is_object() && spec.get("type").is_none() && spec.get("url").is_some())
    {
        spec["type"] = json!("http");
    } else if spec.is_object() && spec.get("type").is_none() && spec.get("command").is_some() {
        spec["type"] = json!("stdio");
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn agy_mcp_preserves_options_and_distinguishes_disable_from_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_config.json");
        fs::write(&path, r#"{"extra":1,"mcpServers":{"unmanaged":{"command":"keep"},"managed":{"command":"old","args":["old"],"timeout":3000}}}"#).unwrap();
        sync_file(&path,"managed",AgyChange::Upsert(&json!({"type":"http","url":"https://example.test/mcp","headers":{"X-Test":"test"}}))).unwrap();
        let doc = read(&path).unwrap();
        assert_eq!(doc["extra"], 1);
        assert_eq!(doc["mcpServers"]["unmanaged"]["command"], "keep");
        let server = &doc["mcpServers"]["managed"];
        assert_eq!(server["timeout"], 3000);
        assert_eq!(server["disabled"], false);
        assert_eq!(server["headers"]["X-Test"], "test");
        assert!(server.get("command").is_none());
        assert!(server.get("args").is_none());
        assert!(server.get("type").is_none());
        sync_file(&path, "managed", AgyChange::Disable).unwrap();
        assert_eq!(
            read(&path).unwrap()["mcpServers"]["managed"]["disabled"],
            true
        );
        sync_file(&path, "managed", AgyChange::Remove).unwrap();
        assert!(read(&path).unwrap()["mcpServers"].get("managed").is_none());
    }
    #[test]
    fn agy_rejects_broken_config_and_sse_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_config.json");
        for original in [
            "{broken",
            r#"{"mcpServers":[]}"#,
            r#"{"mcpServers":{"m":null}}"#,
        ] {
            fs::write(&path, original).unwrap();
            assert!(sync_file(&path, "m", AgyChange::Disable).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        fs::write(&path, "{}").unwrap();
        assert!(sync_file(
            &path,
            "m",
            AgyChange::Upsert(&json!({"type":"sse","url":"https://example.test/sse"}))
        )
        .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
    }
    #[test]
    fn agy_treats_the_empty_file_agy_creates_as_no_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_config.json");
        fs::write(&path, "").unwrap();
        sync_file(&path, "m", AgyChange::Upsert(&json!({"command":"new"}))).unwrap();
        assert_eq!(read(&path).unwrap()["mcpServers"]["m"]["command"], "new");
    }
    #[test]
    fn agy_mcp_restores_native_file_when_database_commit_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_config.json");
        let original = r#"{"mcpServers":{"m":{"command":"old"}}}"#;
        fs::write(&path, original).unwrap();
        let result: Result<(), AppError> = crate::mcode_config::write_and_commit(
            &path,
            || sync_file(&path, "m", AgyChange::Upsert(&json!({"command":"new"}))),
            || Err(AppError::Database("fixture failure".into())),
        );
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
    #[test]
    fn agy_normalizes_legacy_url_for_import() {
        let normalized =
            unified_spec(&json!({"serverUrl":"https://example.test/mcp","disabled":true}));
        assert_eq!(normalized["url"], "https://example.test/mcp");
        assert_eq!(normalized["type"], "http");
        assert!(normalized.get("serverUrl").is_none());
    }
}
