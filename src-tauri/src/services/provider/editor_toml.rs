//! TOML 配置（Codex、Grok Build 的 `config.toml`）编辑器的共用部分：把配置拆成可以单独
//! 改动的位置，算出打开编辑器之后用户改了哪些全局设置，再按三方比较写进 live。
//!
//! 哪些位置算全局设置由各应用的编辑器决定（关键字段、CC Switch 写的表不算）。

use std::path::Path;

use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::error::AppError;
use crate::live::patch::toml::TomlDocPatch;
use crate::live::patch::LiveWriteError;

use super::claude_editor::{ConflictPolicy, InactiveField};

/// 行（或编辑器内容）里的 TOML 文本。
pub(crate) fn config_text(settings: &Value) -> &str {
    settings.get("config").and_then(Value::as_str).unwrap_or("")
}

/// 解析编辑器里的 TOML。`key`、`app` 用来报错，`what` 说明是哪一份。
pub(crate) fn parse_text(
    text: &str,
    key: &'static str,
    app: &str,
    what: &str,
) -> Result<DocumentMut, AppError> {
    text.parse::<DocumentMut>().map_err(|err| {
        AppError::localized(
            key,
            format!("{app} 配置不是合法的 TOML（{what}）：{err}"),
            format!("The {app} configuration is not valid TOML ({what}): {err}"),
        )
    })
}

/// 行里保存着、但不随切换生效的全局设置（值和显示的不同才列出）。值是可以照抄的 TOML。
pub(crate) fn inactive_fields(
    row_entries: Vec<Entry>,
    display: &DocumentMut,
) -> Vec<InactiveField> {
    row_entries
        .into_iter()
        .filter(|entry| item_at(display, &entry.path).map(render) != Some(render(&entry.item)))
        .map(|entry| {
            let mut fragment = DocumentMut::new();
            insert_at(&mut fragment, &entry.path, entry.item.clone());
            InactiveField {
                path: entry.path,
                value: Value::String(fragment.to_string()),
            }
        })
        .collect()
}

/// 一个可以单独改动的位置和它的内容。
pub(crate) struct Entry {
    pub path: Vec<String>,
    pub item: Item,
}

/// 比较用的规范文本：不看两侧的空白和注释。
pub(crate) fn render(item: &Item) -> String {
    let mut doc = DocumentMut::new();
    let mut item = item.clone();
    match &mut item {
        Item::Value(value) => value.decor_mut().clear(),
        Item::Table(table) => {
            table.decor_mut().clear();
            table.set_implicit(false);
        }
        _ => {}
    }
    doc.insert("v", item);
    doc.to_string().trim().to_string()
}

pub(crate) fn item_at<'a>(doc: &'a DocumentMut, path: &[String]) -> Option<&'a Item> {
    let (last, parents) = path.split_last()?;
    let mut current: &dyn TableLike = doc.as_table();
    for segment in parents {
        current = current.get(segment)?.as_table_like()?;
    }
    current.get(last)
}

/// 编辑器里对全局设置的一处改动。
#[derive(Debug, Clone)]
struct Change {
    path: Vec<String>,
    /// 打开编辑器时的内容；`None` 表示当时没有。
    before: Option<String>,
    /// 保存的内容；`None` 表示删掉。
    after: Option<Item>,
}

/// 一次编辑器保存要写进 live 的全局改动。
#[derive(Debug, Clone)]
pub(crate) struct TomlEdits {
    changes: Vec<Change>,
    on_conflict: ConflictPolicy,
}

impl TomlEdits {
    /// 从打开编辑器时和保存时的全局设置算出改动。
    pub(crate) fn between(base: &[Entry], edited: &[Entry], on_conflict: ConflictPolicy) -> Self {
        Self {
            changes: changes(base, edited),
            on_conflict,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// 改动的位置（`a.b`），按顺序。
    #[cfg(test)]
    pub(crate) fn paths(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|change| change.path.join("."))
            .collect()
    }

    /// 按三方比较把改动应用到 live 文档上。
    pub(crate) fn apply_to(
        &self,
        path: &Path,
        doc: &mut DocumentMut,
    ) -> Result<(), LiveWriteError> {
        let accepted = self.on_conflict.resolve(path, &self.changes, |change| {
            let current = item_at(doc, &change.path).map(render);
            let after = change.after.as_ref().map(render);
            (current != change.before && current != after).then(|| change.path.join("."))
        })?;
        for change in accepted {
            match &change.after {
                Some(item) => insert_at(doc, &change.path, item.clone()),
                None => remove_at(doc, &change.path),
            }
        }
        Ok(())
    }
}

impl TomlDocPatch for TomlEdits {
    fn apply_to(&self, path: &Path, doc: &mut DocumentMut) -> Result<(), LiveWriteError> {
        Self::apply_to(self, path, doc)
    }
}

pub(crate) fn insert_at(doc: &mut DocumentMut, path: &[String], item: Item) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    // 内联表（`model_providers = { … }`）里只能放值：表要转成内联表。
    let mut inline = false;
    for segment in parents {
        if current.get(segment).and_then(Item::as_table_like).is_none() {
            current.insert(segment, Item::Table(Table::new()));
        }
        let child = current.get_mut(segment).expect("just ensured a table");
        inline = matches!(child, Item::Value(_));
        current = child.as_table_like_mut().expect("just ensured a table");
    }
    let item = match item {
        Item::Table(table) if inline => {
            Item::Value(toml_edit::Value::InlineTable(table.into_inline_table()))
        }
        other => other,
    };
    match current.get_mut(last) {
        Some(slot) => {
            let decor = match &*slot {
                Item::Value(value) => Some(value.decor().clone()),
                _ => None,
            };
            *slot = item;
            if let (Some(decor), Item::Value(value)) = (decor, slot) {
                *value.decor_mut() = decor;
            }
        }
        None => {
            current.insert(last, item);
        }
    }
}

pub(crate) fn remove_at(doc: &mut DocumentMut, path: &[String]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for segment in parents {
        let Some(next) = current.get_mut(segment).and_then(Item::as_table_like_mut) else {
            return;
        };
        current = next;
    }
    current.remove(last);
}

fn changes(base_entries: &[Entry], edited_entries: &[Entry]) -> Vec<Change> {
    let render_of = |entries: &[Entry], path: &[String]| {
        entries
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| (render(&entry.item), entry.item.clone()))
    };
    let mut paths: Vec<Vec<String>> = base_entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    for entry in edited_entries {
        if !paths.contains(&entry.path) {
            paths.push(entry.path.clone());
        }
    }
    paths
        .into_iter()
        .filter_map(|path| {
            let before = render_of(base_entries, &path);
            let after = render_of(edited_entries, &path);
            if before.as_ref().map(|(text, _)| text) == after.as_ref().map(|(text, _)| text) {
                return None;
            }
            Some(Change {
                path,
                before: before.map(|(text, _)| text),
                after: after.map(|(_, item)| item),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> DocumentMut {
        text.parse::<DocumentMut>().expect("test TOML must parse")
    }

    fn path_of(path: &[&str]) -> Vec<String> {
        path.iter().map(|segment| segment.to_string()).collect()
    }

    /// 以 `v` 为键的值条目，供比较用。
    fn value_from(text: &str) -> Item {
        doc(text).get("v").unwrap().clone()
    }

    fn entry(path: &[&str], item_text: &str) -> Entry {
        Entry {
            path: path_of(path),
            item: value_from(item_text),
        }
    }

    #[test]
    fn config_text_reads_the_inline_toml_or_defaults_empty() {
        assert_eq!(
            config_text(&serde_json::json!({"config": "a = 1"})),
            "a = 1"
        );
        assert_eq!(config_text(&serde_json::json!({})), "");
        assert_eq!(config_text(&serde_json::json!({"config": 5})), "");
        assert_eq!(config_text(&serde_json::json!({"other": "a = 1"})), "");
    }

    #[test]
    fn parse_text_names_the_app_and_the_source_in_the_error() {
        let parsed = parse_text("a = 1", "codex.config", "Codex", "row").unwrap();
        assert_eq!(parsed["a"].as_integer(), Some(1));

        let error = parse_text("a = =", "codex.config", "Codex", "row").unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Codex 配置不是合法的 TOML（row）："),
            "{error}"
        );
        // 本地化键要留着，前端可按 key 取英文消息。
        match error {
            AppError::Localized { key, zh, en } => {
                assert_eq!(key, "codex.config");
                assert!(zh.contains("（row）"), "{zh}");
                assert!(en.contains("(row)"), "{en}");
            }
            other => panic!("expected Localized, got {other:?}"),
        }
    }

    #[test]
    fn render_canonicalizes_values_and_only_the_top_level_of_a_table() {
        // 值：装饰整层清掉，键统一换成 "v"。
        assert_eq!(render(&value_from("v = 1  # trailing\n")), "v = 1");
        assert_eq!(render(&value_from("\n\nv = 1\n")), "v = 1");

        // 表：只清表自己那层的装饰；子值上的注释会原样留着。
        let table = doc("[t]\nname = \"R\"\n").get("t").unwrap().clone();
        assert_eq!(render(&table), "[v]\nname = \"R\"");
        let commented = doc("[t]\nname = \"R\"  # note\n").get("t").unwrap().clone();
        assert_eq!(render(&commented), "[v]\nname = \"R\"  # note");
    }

    #[test]
    fn item_at_walks_tables_inline_tables_and_reports_misses() {
        let live = doc("[model_providers.custom]\nname = \"Relay\"\n\n\
             [model_providers.custom.http_headers]\nX-Key = \"1\"\n");
        let deep = item_at(
            &live,
            &path_of(&["model_providers", "custom", "http_headers", "X-Key"]),
        )
        .unwrap();
        assert_eq!(render(deep), "v = \"1\"");

        // 空路径、缺键、中间段是值都返回 None。
        assert!(item_at(&live, &[]).is_none());
        assert!(item_at(&live, &path_of(&["model_providers", "custom", "nope"])).is_none());
        assert!(item_at(&live, &path_of(&["name", "deeper", "nope"])).is_none());

        let inline = doc("model_providers = { custom = { name = \"Inline\" } }\n");
        assert_eq!(
            render(item_at(&inline, &path_of(&["model_providers", "custom", "name"])).unwrap()),
            "v = \"Inline\""
        );
    }

    #[test]
    fn insert_at_creates_missing_parents_and_inlines_inside_inline_tables() {
        let mut live = doc("x = 0\n");
        insert_at(
            &mut live,
            &path_of(&["model_providers", "custom", "name"]),
            value_from("v = 1"),
        );
        let text = live.to_string();
        assert!(text.contains("[model_providers]"), "{text}");
        assert!(text.contains("[model_providers.custom]"), "{text}");
        assert!(text.contains("name = 1"), "{text}");

        // 空路径什么都不做。
        let snapshot = live.to_string();
        insert_at(&mut live, &[], value_from("v = 1"));
        assert_eq!(live.to_string(), snapshot);

        let mut inline = doc("model_providers = { custom = { name = 0 } }\n");
        let sub = doc("[http_headers]\nX-Key = \"1\"\n")
            .get("http_headers")
            .unwrap()
            .clone();
        insert_at(
            &mut inline,
            &path_of(&["model_providers", "custom", "http_headers"]),
            sub,
        );
        assert!(
            inline.to_string().contains("http_headers = {"),
            "tables inside inline tables must stay inline: {}",
            inline
        );
    }

    #[test]
    fn insert_at_replacing_a_value_keeps_the_old_decor() {
        let mut live = doc("name = \"old\"  # keep me\n");
        insert_at(&mut live, &path_of(&["name"]), value_from("v = \"new\""));
        let text = live.to_string();
        assert!(text.contains("\"new\""), "{text}");
        assert!(text.contains("# keep me"), "{text}");
    }

    #[test]
    fn remove_at_drops_the_key_and_ignores_missing_paths() {
        let mut live = doc("[t]\na = 1\nb = 2\n");
        remove_at(&mut live, &path_of(&["t", "a"]));
        assert!(live.get("t").unwrap().get("a").is_none());
        assert_eq!(live["t"]["b"].as_integer(), Some(2));

        let snapshot = live.to_string();
        remove_at(&mut live, &path_of(&["t", "zzz"]));
        remove_at(&mut live, &path_of(&["zzz", "a"]));
        remove_at(&mut live, &[]);
        assert_eq!(live.to_string(), snapshot);
    }

    #[test]
    fn changes_come_out_in_base_order_then_editor_only_paths() {
        let base = vec![entry(&["m"], "v = \"1\""), entry(&["keep"], "v = \"x\"")];
        let edited = vec![
            entry(&["keep"], "v = \"x\""),
            entry(&["m"], "v = \"2\""),
            entry(&["zeta"], "v = \"9\""),
        ];
        let edits = TomlEdits::between(&base, &edited, ConflictPolicy::KeepMine);
        assert!(!edits.is_empty());
        assert_eq!(edits.paths(), vec!["m".to_string(), "zeta".to_string()]);

        // 两侧一致的条目不动。
        assert!(TomlEdits::between(&base, &base, ConflictPolicy::KeepMine).is_empty());
    }

    #[test]
    fn entries_that_disappear_from_the_editor_become_removals() {
        let base = vec![
            entry(&["model"], "v = \"old\""),
            entry(&["gone"], "v = \"bye\""),
        ];
        let edits = TomlEdits::between(&base, &[], ConflictPolicy::KeepMine);
        assert_eq!(edits.paths(), vec!["model".to_string(), "gone".to_string()]);

        let mut live = doc("model = \"old\"\ngone = \"bye\"\n");
        edits
            .apply_to(Path::new("/data/config.toml"), &mut live)
            .unwrap();
        assert!(live.get("model").is_none());
        assert!(live.get("gone").is_none());
    }

    #[test]
    fn apply_to_carries_editor_edits_into_the_live_document() {
        let base = vec![entry(&["model"], "v = \"old\"")];
        let edited = vec![entry(&["model"], "v = \"new\"")];
        let edits = TomlEdits::between(&base, &edited, ConflictPolicy::KeepMine);

        let mut live = doc("model = \"old\"\n");
        edits
            .apply_to(Path::new("/data/config.toml"), &mut live)
            .unwrap();
        assert_eq!(live["model"].as_str(), Some("new"));

        // live 被人改过时 KeepTheirs 跳过这处改动。
        let keep_theirs = TomlEdits::between(&base, &edited, ConflictPolicy::KeepTheirs);
        let mut foreign = doc("model = \"foreign\"\n");
        keep_theirs
            .apply_to(Path::new("/data/config.toml"), &mut foreign)
            .unwrap();
        assert_eq!(foreign["model"].as_str(), Some("foreign"));
    }

    #[test]
    fn apply_to_refuses_the_whole_save_on_a_conflicting_change() {
        let base = vec![entry(&["model"], "v = \"old\"")];
        let edited = vec![entry(&["model"], "v = \"new\"")];
        let edits = TomlEdits::between(&base, &edited, ConflictPolicy::Refuse);

        let mut live = doc("model = \"foreign\"\n");
        let error = edits
            .apply_to(Path::new("/data/config.toml"), &mut live)
            .unwrap_err();
        match error {
            LiveWriteError::EditConflict { path, keys } => {
                assert_eq!(path, Path::new("/data/config.toml"));
                assert_eq!(keys, vec!["model".to_string()]);
            }
            other => panic!("expected EditConflict, got {other:?}"),
        }
        // 拒绝时一个键都不写。
        assert_eq!(live["model"].as_str(), Some("foreign"));
    }

    #[test]
    fn inactive_fields_lists_row_values_the_display_no_longer_shows() {
        let row = vec![
            entry(&["env", "KEY"], "v = \"row\"  # from row\n"),
            entry(&["model"], "v = \"same\""),
        ];
        let display = doc("env = { KEY = \"live\" }\nmodel = \"same\"\n");

        let fields = inactive_fields(row, &display);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path, path_of(&["env", "KEY"]));
        // 值是可以照抄的 TOML 片段，键路径齐全。
        let fragment = fields[0].value.as_str().unwrap();
        assert!(fragment.contains("KEY = \"row\""), "{fragment}");
        let parsed: DocumentMut = fragment.parse().expect("fragment must parse");
        assert_eq!(
            render(item_at(&parsed, &path_of(&["env", "KEY"])).unwrap()),
            "v = \"row\""
        );
    }
}
