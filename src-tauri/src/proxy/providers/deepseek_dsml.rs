//! Inline tool-call text recovery and reasoning-continuation cleanup.
//!
//! NVIDIA NIM and a few other OpenAI-compatible upstreams can leak DeepSeek's
//! native DSML markup into `content` / `reasoning_content` when their server
//! side tool parser is not fully enabled. The leaked text is not a valid
//! OpenAI payload and must never reach the client as visible message text.
//! This is a unified rule for every Chat upstream: the filter is driven by the
//! response text itself, not by a provider or model name.
//!
//! Supported variants:
//! - DeepSeek V4: `<pipe>DSML<pipe>tool_calls`, `<pipe>DSML<pipe>invoke name="`
//! - DeepSeek V4.1 spaced form: `<pipe>DSML<pipe> calls`, `<pipe>DSML<pipe> invoke name="`
//! - ASCII pipe fallback for both variants.
//! - The `<pipe>DSML<pipe>:ToolName` colon form seen from V4 Flash rollouts.
//!
//! Complete blocks are converted into OpenAI `tool_calls`; stray openers,
//! closers and parser-constant fragments are stripped from pass-through text.

use serde_json::{json, Map, Value};

/// Full-width vertical line first, ASCII fallback second.
const PIPE_VARIANTS: [&str; 2] = ["｜", "|"];

/// `<\u{2F}` keeps the source free of a literal closing-tag sequence.
const CLOSE_START: &str = "<\u{2F}";

const WRAPPER_OPENERS: [&str; 12] = [
    "<｜DSML｜tool_calls>",
    "<|DSML|tool_calls>",
    "<｜DSML｜toolcalls>",
    "<|DSML|toolcalls>",
    "<｜DSML｜tool>",
    "<|DSML|tool>",
    "<｜DSML｜ calls>",
    "<|DSML| calls>",
    "<｜DSML｜ toolcalls>",
    "<|DSML| toolcalls>",
    "<｜DSML｜ tool>",
    "<|DSML| tool>",
];

const WRAPPER_CLOSERS: [&str; 12] = [
    concat!("<\u{2F}", "｜DSML｜tool_calls>"),
    concat!("<\u{2F}", "|DSML|tool_calls>"),
    concat!("<\u{2F}", "｜DSML｜toolcalls>"),
    concat!("<\u{2F}", "|DSML|toolcalls>"),
    concat!("<\u{2F}", "｜DSML｜tool>"),
    concat!("<\u{2F}", "|DSML|tool>"),
    concat!("<\u{2F}", "｜DSML｜ calls>"),
    concat!("<\u{2F}", "|DSML| calls>"),
    concat!("<\u{2F}", "｜DSML｜ toolcalls>"),
    concat!("<\u{2F}", "|DSML| toolcalls>"),
    concat!("<\u{2F}", "｜DSML｜ tool>"),
    concat!("<\u{2F}", "|DSML| tool>"),
];

const INVOKE_OPEN_PREFIXES: [&str; 4] = [
    "<｜DSML｜invoke name=\"",
    "<|DSML|invoke name=\"",
    "<｜DSML｜ invoke name=\"",
    "<|DSML| invoke name=\"",
];

const INVOKE_CLOSERS: [&str; 4] = [
    concat!("<\u{2F}", "｜DSML｜invoke>"),
    concat!("<\u{2F}", "|DSML|invoke>"),
    concat!("<\u{2F}", "｜DSML｜ invoke>"),
    concat!("<\u{2F}", "|DSML| invoke>"),
];

const PARAM_OPEN_PREFIXES: [&str; 4] = [
    "<｜DSML｜parameter",
    "<|DSML|parameter",
    "<｜DSML｜ parameter",
    "<|DSML| parameter",
];

const PARAM_CLOSERS: [&str; 4] = [
    concat!("<\u{2F}", "｜DSML｜parameter>"),
    concat!("<\u{2F}", "|DSML|parameter>"),
    concat!("<\u{2F}", "｜DSML｜ parameter>"),
    concat!("<\u{2F}", "|DSML| parameter>"),
];

/// Parser constant fragments observed leaking from NIM DeepSeek V4.1.
const SCAFFOLDING_TOKENS: [&str; 12] = [
    "_INVOKE_PREFIX = '",
    "DSML_INVOKE_PREFIX = '",
    "INVOKE_PREFIX = '",
    "invoke_prefix = '",
    "_INVOKE_PREFIX = \"",
    "DSML_INVOKE_PREFIX = \"",
    "DSML_TOOL_START = \"",
    "DSML_TOOL_END = \"",
    "DSML_INVOKE_END = \"",
    "DSML_PARAM_START = \"",
    "DSML_PARAM_CLOSE = \"",
    "DSML_INVOKE_NAME_END = \"",
];

const REMOVABLE_MARKERS: [&str; 34] = [
    concat!("<\u{2F}", "｜DSML｜tool_calls>"),
    concat!("<\u{2F}", "|DSML|tool_calls>"),
    concat!("<\u{2F}", "｜DSML｜toolcalls>"),
    concat!("<\u{2F}", "|DSML|toolcalls>"),
    concat!("<\u{2F}", "｜DSML｜tool>"),
    concat!("<\u{2F}", "|DSML|tool>"),
    concat!("<\u{2F}", "｜DSML｜ calls>"),
    concat!("<\u{2F}", "|DSML| calls>"),
    concat!("<\u{2F}", "｜DSML｜ toolcalls>"),
    concat!("<\u{2F}", "|DSML| toolcalls>"),
    concat!("<\u{2F}", "｜DSML｜ tool>"),
    concat!("<\u{2F}", "|DSML| tool>"),
    concat!("<\u{2F}", "｜DSML｜invoke>"),
    concat!("<\u{2F}", "|DSML|invoke>"),
    concat!("<\u{2F}", "｜DSML｜ invoke>"),
    concat!("<\u{2F}", "|DSML| invoke>"),
    concat!("<\u{2F}", "｜DSML｜parameter>"),
    concat!("<\u{2F}", "|DSML|parameter>"),
    concat!("<\u{2F}", "｜DSML｜ parameter>"),
    concat!("<\u{2F}", "|DSML| parameter>"),
    concat!("<\u{2F}", "parameter>"),
    concat!("<\u{2F}", "invoke>"),
    "_INVOKE_PREFIX = '",
    "DSML_INVOKE_PREFIX = '",
    "INVOKE_PREFIX = '",
    "invoke_prefix = '",
    "_INVOKE_PREFIX = \"",
    "DSML_INVOKE_PREFIX = \"",
    "DSML_TOOL_START = \"",
    "DSML_TOOL_END = \"",
    "DSML_INVOKE_END = \"",
    "DSML_PARAM_START = \"",
    "DSML_PARAM_CLOSE = \"",
    "DSML_INVOKE_NAME_END = \"",
];

const MAX_DSML_BLOCK_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DsmlToolCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DsmlParseResult {
    pub content: String,
    pub tool_calls: Vec<DsmlToolCall>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DsmlStreamOutput {
    pub text: Option<String>,
    pub tool_calls: Vec<DsmlToolCall>,
}

#[derive(Debug)]
enum OpenerKind {
    Wrapper,
    Invoke,
    Colon { name: String, pipe: &'static str },
}

#[derive(Debug)]
struct OpenerMatch {
    index: usize,
    len: usize,
    kind: OpenerKind,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum DsmlStreamMode {
    #[default]
    Text,
    Wrapper,
    Invoke,
    Colon {
        name: String,
        pipe: &'static str,
    },
}

fn push_text(out: &mut DsmlStreamOutput, text: &str) {
    if text.is_empty() {
        return;
    }
    out.text.get_or_insert_with(String::new).push_str(text);
}

fn find_earliest(text: &str, markers: &[&str]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for marker in markers {
        if let Some(index) = text.find(marker) {
            if best.is_none_or(|(best_index, _)| index < best_index) {
                best = Some((index, marker.len()));
            }
        }
    }
    best
}

fn consider_opener(best: &mut Option<OpenerMatch>, index: usize, len: usize, kind: OpenerKind) {
    if best.as_ref().is_none_or(|current| index < current.index) {
        *best = Some(OpenerMatch { index, len, kind });
    }
}

fn find_colon_opener(text: &str) -> Option<(usize, usize, String, &'static str)> {
    let mut best: Option<(usize, usize, String, &'static str)> = None;
    for pipe in PIPE_VARIANTS {
        let prefix = format!("<{pipe}DSML{pipe}:");
        let mut search_from = 0;
        while let Some(relative) = text[search_from..].find(&prefix) {
            let start = search_from + relative;
            let name_start = start + prefix.len();
            let Some(gt) = text[name_start..].find('>') else {
                break;
            };
            let name = &text[name_start..name_start + gt];
            if !name.is_empty() && !name.contains('<') {
                let candidate = (start, prefix.len() + gt + 1, name.to_string(), pipe);
                if best.as_ref().is_none_or(|current| candidate.0 < current.0) {
                    best = Some(candidate);
                }
            }
            search_from = name_start + gt + 1;
        }
    }
    best
}

fn find_next_opener(text: &str) -> Option<OpenerMatch> {
    let mut best: Option<OpenerMatch> = None;
    for marker in WRAPPER_OPENERS {
        if let Some(index) = text.find(marker) {
            consider_opener(&mut best, index, marker.len(), OpenerKind::Wrapper);
        }
    }
    for marker in INVOKE_OPEN_PREFIXES {
        if let Some(index) = text.find(marker) {
            consider_opener(&mut best, index, marker.len(), OpenerKind::Invoke);
        }
    }
    if let Some((index, len, name, pipe)) = find_colon_opener(text) {
        consider_opener(&mut best, index, len, OpenerKind::Colon { name, pipe });
    }
    best
}

fn find_next_removable(text: &str) -> Option<(usize, usize)> {
    find_earliest(text, &REMOVABLE_MARKERS)
}

fn colon_closer(text: &str, name: &str) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for pipe in PIPE_VARIANTS {
        let marker = format!("{CLOSE_START}{pipe}DSML{pipe}:{name}>");
        if let Some(index) = text.find(&marker) {
            if best.is_none_or(|(best_index, _)| index < best_index) {
                best = Some((index, marker.len()));
            }
        }
    }
    best
}

fn attr_value(attrs: &str, key: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(relative) = attrs[search_from..].find(key) {
        let index = search_from + relative;
        let boundary = attrs[..index]
            .chars()
            .next_back()
            .is_none_or(|ch| ch.is_whitespace() || ch == '<');
        if boundary {
            let after = attrs[index + key.len()..].trim_start();
            if let Some(after) = after.strip_prefix('=') {
                let after = after.trim_start();
                for quote in ['"', '\''] {
                    if let Some(after) = after.strip_prefix(quote) {
                        if let Some(end) = after.find(quote) {
                            return Some(after[..end].to_string());
                        }
                    }
                }
            }
        }
        search_from = index + key.len();
    }
    None
}

fn dsml_value(raw: &str, is_string: bool) -> Value {
    if is_string {
        return Value::String(raw.to_string());
    }
    serde_json::from_str(raw.trim()).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn parse_parameters(body: &str) -> Map<String, Value> {
    let mut args = Map::new();
    let mut rest = body;
    while let Some((index, len)) = find_earliest(rest, &PARAM_OPEN_PREFIXES) {
        let after_prefix = &rest[index + len..];
        let Some(gt) = after_prefix.find('>') else {
            break;
        };
        let attrs = &after_prefix[..gt];
        let value_start = index + len + gt + 1;
        let Some((close_index, close_len)) = find_earliest(&rest[value_start..], &PARAM_CLOSERS)
        else {
            break;
        };
        let value = &rest[value_start..value_start + close_index];
        if let Some(name) = attr_value(attrs, "name") {
            let is_string = attr_value(attrs, "string").as_deref() == Some("true");
            args.insert(name, dsml_value(value, is_string));
        }
        rest = &rest[value_start + close_index + close_len..];
    }
    args
}

fn parse_unprefixed_parameters(body: &str) -> Map<String, Value> {
    let mut args = Map::new();
    let mut rest = body;
    while let Some(index) = rest.find("<parameter") {
        let after = &rest[index + "<parameter".len()..];
        let Some(gt) = after.find('>') else {
            break;
        };
        let attrs = &after[..gt];
        let value_start = index + "<parameter".len() + gt + 1;
        let Some(close) = rest[value_start..].find(concat!("<\u{2F}", "parameter>")) else {
            break;
        };
        let value = &rest[value_start..value_start + close];
        if let Some(name) = attr_value(attrs, "name") {
            let is_string = attr_value(attrs, "string").as_deref() == Some("true");
            args.insert(name, dsml_value(value, is_string));
        }
        rest = &rest[value_start + close + concat!("<\u{2F}", "parameter>").len()..];
    }
    args
}

fn parse_bare_child_tags(body: &str) -> Map<String, Value> {
    let mut args = Map::new();
    let mut search_from = 0;
    while let Some(open_relative) = body[search_from..].find('<') {
        let open = search_from + open_relative;
        let Some(tag_end_relative) = body[open..].find('>') else {
            break;
        };
        let tag_end = open + tag_end_relative;
        let tag = &body[open + 1..tag_end];
        if tag.is_empty() || tag.starts_with('/') || tag.contains(char::is_whitespace) {
            search_from = tag_end + 1;
            continue;
        }
        let close = format!("{CLOSE_START}{tag}>");
        let Some(close_relative) = body[tag_end + 1..].find(&close) else {
            search_from = tag_end + 1;
            continue;
        };
        let value_start = tag_end + 1;
        let value = &body[value_start..value_start + close_relative];
        if !matches!(tag, "parameter" | "invoke" | "tool_calls" | "toolcalls") {
            args.insert(tag.to_string(), Value::String(value.to_string()));
        }
        search_from = value_start + close_relative + close.len();
    }
    args
}

fn parse_colon_arguments(body: &str) -> Map<String, Value> {
    let named = parse_unprefixed_parameters(body);
    if !named.is_empty() {
        return named;
    }
    parse_bare_child_tags(body)
}

fn args_to_json(args: Map<String, Value>) -> String {
    serde_json::to_string(&Value::Object(args)).unwrap_or_else(|_| "{}".to_string())
}

fn parse_invokes(mut rest: &str, calls: &mut Vec<DsmlToolCall>) {
    while let Some((index, len)) = find_earliest(rest, &INVOKE_OPEN_PREFIXES) {
        let after_prefix = &rest[index + len..];
        let Some(name_end) = after_prefix.find('"') else {
            break;
        };
        let Some(gt_relative) = after_prefix[name_end..].find('>') else {
            break;
        };
        let gt = name_end + gt_relative;
        let name = &after_prefix[..name_end];
        let body = &after_prefix[gt + 1..];
        let Some((close_index, close_len)) = find_earliest(body, &INVOKE_CLOSERS) else {
            break;
        };
        let name = name.trim();
        if !name.is_empty() {
            calls.push(DsmlToolCall {
                name: name.to_string(),
                arguments: args_to_json(parse_parameters(&body[..close_index])),
            });
        }
        rest = &body[close_index + close_len..];
    }
}

/// Parse complete DSML blocks out of a Chat Completions text field.
pub(crate) fn parse_dsml_tool_calls(text: &str) -> DsmlParseResult {
    let mut result = DsmlParseResult::default();
    let mut remaining = text;

    loop {
        let opener = find_next_opener(remaining);
        let removable = find_next_removable(remaining);

        match (opener, removable) {
            (None, None) => {
                result.content.push_str(&scrub_diverged_markers(remaining));
                break;
            }
            (None, Some((index, len))) => {
                result
                    .content
                    .push_str(&scrub_diverged_markers(&remaining[..index]));
                remaining = &remaining[index + len..];
            }
            (Some(opener), Some((index, len))) if index < opener.index => {
                result
                    .content
                    .push_str(&scrub_diverged_markers(&remaining[..index]));
                remaining = &remaining[index + len..];
            }
            (Some(opener), _) => {
                result
                    .content
                    .push_str(&scrub_diverged_markers(&remaining[..opener.index]));
                let after_opener = &remaining[opener.index + opener.len..];
                match opener.kind {
                    OpenerKind::Wrapper => {
                        let Some((index, len)) = find_earliest(after_opener, &WRAPPER_CLOSERS)
                        else {
                            log::warn!(
                                "[DSML] dropped unterminated tool_calls block ({} bytes)",
                                after_opener.len()
                            );
                            parse_invokes(after_opener, &mut result.tool_calls);
                            let salvaged = salvage_unterminated_block(after_opener);
                            if let Some(text) = salvaged.text {
                                result.content.push_str(&text);
                            }
                            break;
                        };
                        parse_invokes(&after_opener[..index], &mut result.tool_calls);
                        remaining = &after_opener[index + len..];
                    }
                    OpenerKind::Invoke => {
                        let Some(name_end) = after_opener.find('"') else {
                            log::warn!("[DSML] dropped unterminated invoke opener");
                            break;
                        };
                        let Some(gt_relative) = after_opener[name_end..].find('>') else {
                            log::warn!("[DSML] dropped unterminated invoke opener");
                            break;
                        };
                        let gt = name_end + gt_relative;
                        let name = &after_opener[..name_end];
                        let body = &after_opener[gt + 1..];
                        let Some((index, len)) = find_earliest(body, &INVOKE_CLOSERS) else {
                            log::warn!(
                                "[DSML] dropped unterminated invoke block ({} bytes)",
                                body.len()
                            );
                            let salvaged = salvage_unterminated_block(body);
                            if let Some(text) = salvaged.text {
                                result.content.push_str(&text);
                            }
                            break;
                        };
                        let name = name.trim();
                        if !name.is_empty() {
                            result.tool_calls.push(DsmlToolCall {
                                name: name.to_string(),
                                arguments: args_to_json(parse_parameters(&body[..index])),
                            });
                        }
                        remaining = &body[index + len..];
                    }
                    OpenerKind::Colon { name, pipe } => {
                        let _ = pipe;
                        let Some((index, len)) = colon_closer(after_opener, &name) else {
                            log::warn!(
                                "[DSML] dropped unterminated colon block for {name} ({} bytes)",
                                after_opener.len()
                            );
                            break;
                        };
                        result.tool_calls.push(DsmlToolCall {
                            name: name.clone(),
                            arguments: args_to_json(parse_colon_arguments(&after_opener[..index])),
                        });
                        remaining = &after_opener[index + len..];
                    }
                }
            }
        }
    }

    result
}

fn scrub_scaffolding_fragments(text: &str) -> String {
    let mut output = text.to_string();
    for token in SCAFFOLDING_TOKENS {
        while let Some(index) = output.find(token) {
            output.replace_range(index..index + token.len(), "");
        }
    }
    output
}

fn looks_like_dsml_partial(tail: &str) -> bool {
    let trimmed = tail.trim_start();
    (trimmed.starts_with('<')
        && (trimmed.contains("DSML")
            || trimmed.contains("｜DSML｜")
            || trimmed.contains("DS")
            || trimmed.contains("dsml")))
        || trimmed.contains("INVOKE_PREFIX")
        || trimmed.contains("invoke_prefix")
        || trimmed.contains("DSML_")
}

/// Merge DSML tool calls from a Chat message into its standard `tool_calls`
/// array and remove the corresponding markup from every text field.
pub(crate) fn merge_dsml_tool_calls_into_message(
    message: &mut Value,
    call_id_prefix: &str,
) -> usize {
    let Some(object) = message.as_object_mut() else {
        return 0;
    };
    let mut calls = Vec::new();

    for field in ["content", "reasoning_content", "reasoning"] {
        let Some(value) = object.get_mut(field) else {
            continue;
        };
        match value {
            Value::String(text) => {
                let parsed = parse_dsml_tool_calls(text);
                let cleaned = scrub_scaffolding_fragments(&parsed.content);
                if cleaned != *text || !parsed.tool_calls.is_empty() {
                    *text = cleaned;
                }
                calls.extend(parsed.tool_calls);
            }
            Value::Array(parts) => {
                for part in parts.iter_mut() {
                    let Some(part) = part.as_object_mut() else {
                        continue;
                    };
                    let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
                    if !matches!(part_type, "text" | "output_text" | "reasoning_text") {
                        continue;
                    }
                    let Some(Value::String(text)) = part.get_mut("text") else {
                        continue;
                    };
                    let parsed = parse_dsml_tool_calls(text);
                    let cleaned = scrub_scaffolding_fragments(&parsed.content);
                    if cleaned != *text || !parsed.tool_calls.is_empty() {
                        *text = cleaned;
                    }
                    calls.extend(parsed.tool_calls);
                }
            }
            _ => {}
        }
    }

    if calls.is_empty() {
        return 0;
    }

    let count = calls.len();
    let entry = object
        .entry("tool_calls".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(tool_calls) = entry {
        for (index, call) in calls.into_iter().enumerate() {
            tool_calls.push(json!({
                "id": format!("{call_id_prefix}{index}"),
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": call.arguments
                }
            }));
        }
    }
    count
}

fn is_partial_static_marker(suffix: &str) -> bool {
    WRAPPER_OPENERS
        .iter()
        .chain(INVOKE_OPEN_PREFIXES.iter())
        .chain(PARAM_OPEN_PREFIXES.iter())
        .chain(REMOVABLE_MARKERS.iter())
        .any(|marker| marker.len() > suffix.len() && marker.starts_with(suffix))
}

fn is_partial_colon_marker(suffix: &str) -> bool {
    for pipe in PIPE_VARIANTS {
        let open = format!("<{pipe}DSML{pipe}:");
        let close = format!("{CLOSE_START}{pipe}DSML{pipe}:");
        for prefix in [&open, &close] {
            if prefix.starts_with(suffix) {
                return true;
            }
            if let Some(rest) = suffix.strip_prefix(prefix.as_str()) {
                return rest.is_empty()
                    || (!rest.contains('>')
                        && rest.chars().all(|ch| !ch.is_whitespace() && ch != '<'));
            }
        }
    }
    false
}

/// Minimum bytes a `<` fragment must match before it counts as a truncated
/// DSML marker. Keeps ordinary prose with a stray `<` untouched.
const MIN_MARKER_PREFIX_BYTES: usize = 8;

/// Longest common byte prefix between a fragment and any known marker.
fn longest_marker_prefix(fragment: &str) -> usize {
    let mut best = 0usize;
    let consider = |marker: &str, best: &mut usize| {
        let mut len = 0usize;
        for (marker_byte, fragment_byte) in marker.bytes().zip(fragment.bytes()) {
            if marker_byte != fragment_byte {
                break;
            }
            len += 1;
        }
        if len > *best {
            *best = len;
        }
    };
    for marker in WRAPPER_OPENERS
        .iter()
        .chain(WRAPPER_CLOSERS.iter())
        .chain(INVOKE_OPEN_PREFIXES.iter())
        .chain(INVOKE_CLOSERS.iter())
        .chain(PARAM_OPEN_PREFIXES.iter())
        .chain(PARAM_CLOSERS.iter())
        .chain(SCAFFOLDING_TOKENS.iter())
    {
        consider(marker, &mut best);
    }
    for pipe in PIPE_VARIANTS {
        consider(&format!("<{pipe}DSML{pipe}:"), &mut best);
        consider(&format!("{CLOSE_START}{pipe}DSML{pipe}:"), &mut best);
    }
    best
}

/// Length of a truncated DSML marker fragment that starts here. Only fires
/// when the fragment matched a marker long enough to be sure it is scaffolding
/// and then diverged, so it can never become a marker.
fn diverged_marker_span(fragment: &str) -> Option<usize> {
    let prefix_len = longest_marker_prefix(fragment);
    if prefix_len < MIN_MARKER_PREFIX_BYTES || prefix_len >= fragment.len() {
        return None;
    }
    let mut span = prefix_len;
    for ch in fragment[prefix_len..].chars() {
        if ch == '\n' || ch == '\r' {
            break;
        }
        span += ch.len_utf8();
    }
    Some(span)
}

/// Drop truncated DSML fragments that can never complete (NIM truncates the
/// opener mid-tag and then continues with normal text).
fn scrub_diverged_markers(text: &str) -> String {
    if !text.contains("DSML") && !text.contains("dsml") {
        return text.to_string();
    }
    let mut output = String::new();
    let mut rest = text;
    loop {
        let cut = rest
            .char_indices()
            .find(|(_, ch)| *ch == '<')
            .and_then(|(index, _)| diverged_marker_span(&rest[index..]).map(|span| (index, span)));
        match cut {
            Some((index, span)) => {
                output.push_str(&rest[..index]);
                rest = &rest[index + span..];
            }
            None => {
                output.push_str(rest);
                break;
            }
        }
    }
    output
}

/// Salvage an unterminated block: drop scaffolding lines, keep any real text.
fn salvage_unterminated_block(block: &str) -> DsmlStreamOutput {
    let mut output = DsmlStreamOutput::default();
    let mut text = String::new();
    for line in block.split_inclusive('\n') {
        if line_contains_dsml_marker(line) {
            log::warn!(
                "[DSML] dropped unterminated tool call fragment ({} bytes)",
                line.len()
            );
            continue;
        }
        text.push_str(line);
    }
    let cleaned = scrub_diverged_markers(&text);
    if cleaned.trim().is_empty() {
        log::warn!(
            "[DSML] dropped unterminated streaming block ({} bytes) with no salvageable text",
            block.len()
        );
    } else {
        push_text(&mut output, &cleaned);
    }
    output
}

fn line_contains_dsml_marker(line: &str) -> bool {
    find_next_opener(line).is_some() || find_next_removable(line).is_some()
}

fn partial_marker_suffix_len(buffer: &str) -> usize {
    let max_len = buffer.len().min(64);
    for len in (1..=max_len).rev() {
        let start = buffer.len() - len;
        if !buffer.is_char_boundary(start) {
            continue;
        }
        let suffix = &buffer[start..];
        if is_partial_static_marker(suffix) || is_partial_colon_marker(suffix) {
            return len;
        }
    }
    0
}

/// Incremental, streaming-safe DSML splitter.
#[derive(Debug, Default)]
pub(crate) struct DsmlStreamSplitter {
    mode: DsmlStreamMode,
    buffer: String,
}

impl DsmlStreamSplitter {
    pub(crate) fn has_pending(&self) -> bool {
        !self.buffer.is_empty() || self.mode != DsmlStreamMode::Text
    }

    pub(crate) fn push(&mut self, delta: &str) -> DsmlStreamOutput {
        self.buffer.push_str(delta);
        self.drain()
    }

    pub(crate) fn flush(&mut self) -> DsmlStreamOutput {
        let mut output = self.drain();
        if self.mode == DsmlStreamMode::Text {
            let tail = std::mem::take(&mut self.buffer);
            if looks_like_dsml_partial(&tail) {
                log::warn!(
                    "[DSML] dropped truncated marker tail ({} bytes)",
                    tail.len()
                );
            } else {
                push_text(&mut output, &tail);
            }
        } else {
            let block = std::mem::take(&mut self.buffer);
            let salvaged_calls = parse_dsml_tool_calls(&block).tool_calls;
            output.tool_calls.extend(salvaged_calls);
            let salvaged = salvage_unterminated_block(&block);
            if let Some(text) = salvaged.text {
                push_text(&mut output, &text);
            }
            self.mode = DsmlStreamMode::Text;
        }
        output
    }

    fn drain(&mut self) -> DsmlStreamOutput {
        let mut output = DsmlStreamOutput::default();
        loop {
            match &self.mode {
                DsmlStreamMode::Text => {
                    let opener = find_next_opener(&self.buffer);
                    let removable = find_next_removable(&self.buffer);
                    match (opener, removable) {
                        (Some(opener), Some((index, len))) if index < opener.index => {
                            push_text(&mut output, &self.buffer[..index]);
                            self.buffer.drain(..index + len);
                        }
                        (Some(opener), _) => {
                            push_text(&mut output, &self.buffer[..opener.index]);
                            self.buffer.drain(..opener.index);
                            self.mode = match opener.kind {
                                OpenerKind::Wrapper => DsmlStreamMode::Wrapper,
                                OpenerKind::Invoke => DsmlStreamMode::Invoke,
                                OpenerKind::Colon { name, pipe } => {
                                    DsmlStreamMode::Colon { name, pipe }
                                }
                            };
                            break;
                        }
                        (None, Some((index, len))) => {
                            push_text(&mut output, &self.buffer[..index]);
                            self.buffer.drain(..index + len);
                        }
                        (None, None) => {
                            let holdback = partial_marker_suffix_len(&self.buffer);
                            let holdback_end = self.buffer.len() - holdback;
                            push_text(
                                &mut output,
                                &scrub_diverged_markers(&self.buffer[..holdback_end]),
                            );
                            self.buffer.drain(..holdback_end);
                            break;
                        }
                    }
                }
                DsmlStreamMode::Wrapper => {
                    let Some((index, len)) = find_earliest(&self.buffer, &WRAPPER_CLOSERS) else {
                        if self.buffer.len() > MAX_DSML_BLOCK_BYTES {
                            log::warn!(
                                "[DSML] dropped oversized tool_calls block ({} bytes)",
                                self.buffer.len()
                            );
                            self.buffer.clear();
                            self.mode = DsmlStreamMode::Text;
                        } else {
                            break;
                        }
                        continue;
                    };
                    let block = self.buffer[..index + len].to_string();
                    output
                        .tool_calls
                        .extend(parse_dsml_tool_calls(&block).tool_calls);
                    self.buffer.drain(..index + len);
                    self.mode = DsmlStreamMode::Text;
                }
                DsmlStreamMode::Invoke => {
                    let Some((index, len)) = find_earliest(&self.buffer, &INVOKE_CLOSERS) else {
                        if self.buffer.len() > MAX_DSML_BLOCK_BYTES {
                            log::warn!(
                                "[DSML] dropped oversized invoke block ({} bytes)",
                                self.buffer.len()
                            );
                            self.buffer.clear();
                            self.mode = DsmlStreamMode::Text;
                        } else {
                            break;
                        }
                        continue;
                    };
                    let block = self.buffer[..index + len].to_string();
                    output
                        .tool_calls
                        .extend(parse_dsml_tool_calls(&block).tool_calls);
                    self.buffer.drain(..index + len);
                    self.mode = DsmlStreamMode::Text;
                }
                DsmlStreamMode::Colon { name, pipe } => {
                    let name = name.clone();
                    let pipe = *pipe;
                    let Some((index, len)) = colon_closer(&self.buffer, &name) else {
                        if self.buffer.len() > MAX_DSML_BLOCK_BYTES {
                            log::warn!(
                                "[DSML] dropped oversized colon block ({} bytes)",
                                self.buffer.len()
                            );
                            self.buffer.clear();
                            self.mode = DsmlStreamMode::Text;
                        } else {
                            break;
                        }
                        continue;
                    };
                    let _ = pipe;
                    let block = self.buffer[..index + len].to_string();
                    output
                        .tool_calls
                        .extend(parse_dsml_tool_calls(&block).tool_calls);
                    self.buffer.drain(..index + len);
                    self.mode = DsmlStreamMode::Text;
                }
            }
        }
        output
    }
}

/// True when a reasoning excerpt has not reached a sentence boundary.
pub(crate) fn reasoning_is_open(reasoning: &str) -> bool {
    let trimmed = reasoning.trim_end();
    let mut chars = trimmed.chars().rev();
    let Some(last) = chars.next() else {
        return false;
    };
    if !matches!(last, '.' | '!' | '?' | '。' | '！' | '？') {
        return true;
    }
    matches!(chars.next(), Some('.' | '!' | '?' | '。' | '！' | '？'))
}

/// Very conservative signature for a content fragment that continues an
/// unfinished reasoning stream instead of starting a user-facing answer.
pub(crate) fn looks_like_reasoning_continuation_start(text: &str) -> bool {
    matches!(
        text.trim_start().chars().next(),
        Some('<' | '_' | '`' | '\\')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(pipe: &str, body: &str) -> String {
        format!("<{pipe}DSML{pipe}{body}>")
    }

    fn close(pipe: &str, body: &str) -> String {
        format!("{CLOSE_START}{pipe}DSML{pipe}{body}>")
    }

    fn v41_block(name: &str, params: &[(&str, &str)]) -> String {
        let mut block = format!("{}\n", open("｜", " calls"));
        block.push_str(&format!(
            "{}\n",
            open("｜", &format!(" invoke name=\"{name}\""))
        ));
        for (key, value) in params {
            block.push_str(&format!(
                "{}{}{}\n",
                open("｜", &format!(" parameter name=\"{key}\" string=\"true\"")),
                value,
                close("｜", " parameter")
            ));
        }
        block.push_str(&format!("{}\n", close("｜", " invoke")));
        block.push_str(&close("｜", " calls"));
        block
    }

    fn collect_stream(chunks: &[&str]) -> (String, Vec<DsmlToolCall>) {
        let mut splitter = DsmlStreamSplitter::default();
        let mut text = String::new();
        let mut calls = Vec::new();
        for chunk in chunks {
            let output = splitter.push(chunk);
            if let Some(part) = output.text {
                text.push_str(&part);
            }
            calls.extend(output.tool_calls);
        }
        let output = splitter.flush();
        if let Some(part) = output.text {
            text.push_str(&part);
        }
        calls.extend(output.tool_calls);
        (text, calls)
    }

    #[test]
    fn parses_deepseek_v41_spaced_block() {
        let text = format!(
            "先看一下\n{}\n完了",
            v41_block("exec_command", &[("cmd", "ls -la")])
        );
        let parsed = parse_dsml_tool_calls(&text);
        assert_eq!(parsed.content, "先看一下\n\n完了");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "exec_command");
        let args: Value = serde_json::from_str(&parsed.tool_calls[0].arguments).unwrap();
        assert_eq!(args["cmd"], "ls -la");
    }

    #[test]
    fn parses_ascii_v4_block_with_typed_parameter() {
        let text = format!(
            "{}{}{}{}{}{}{}",
            open("|", "tool_calls"),
            open("|", "invoke name=\"lookup\""),
            open("|", "parameter name=\"limit\" string=\"false\""),
            "5000",
            close("|", "parameter"),
            close("|", "invoke"),
            close("|", "tool_calls"),
        );
        let parsed = parse_dsml_tool_calls(&text);
        assert!(parsed.content.is_empty());
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "lookup");
        let args: Value = serde_json::from_str(&parsed.tool_calls[0].arguments).unwrap();
        assert_eq!(args["limit"], 5000);
    }

    #[test]
    fn parses_colon_form_with_bare_child_tag() {
        let body = format!("<path>a.txt{CLOSE_START}path>");
        let text = format!("{}{}{}", open("｜", ":Read"), body, close("｜", ":Read"));
        let parsed = parse_dsml_tool_calls(&text);
        assert!(parsed.content.is_empty());
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "Read");
        let args: Value = serde_json::from_str(&parsed.tool_calls[0].arguments).unwrap();
        assert_eq!(args["path"], "a.txt");
    }

    #[test]
    fn strips_stray_openers_and_scaffolding_fragments() {
        let stray = format!("\n\n{}", open("｜", " calls"));
        let parsed = parse_dsml_tool_calls(&stray);
        assert!(parsed.content.trim().is_empty());
        assert!(parsed.tool_calls.is_empty());

        let fragment = "_INVOKE_PREFIX = '";
        let parsed = parse_dsml_tool_calls(fragment);
        assert!(parsed.content.is_empty());
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn stream_splitter_is_chunk_independent() {
        let payload = format!("前{}\n后", v41_block("exec_command", &[("cmd", "ls")]));
        let whole = collect_stream(&[payload.as_str()]);
        let chars: Vec<String> = payload.chars().map(|ch| ch.to_string()).collect();
        let pieces: Vec<&str> = chars.iter().map(String::as_str).collect();
        let split = collect_stream(&pieces);

        assert_eq!(whole.0, split.0);
        assert_eq!(whole.1, split.1);
        assert!(!whole.0.contains("DSML"));
        assert!(!whole.0.contains("｜DSML｜"));
        assert_eq!(whole.1.len(), 1);
        assert_eq!(whole.1[0].name, "exec_command");
    }

    #[test]
    fn stream_splitter_holds_partial_marker_and_flushes_plain_prefix() {
        let mut splitter = DsmlStreamSplitter::default();
        let output = splitter.push("hello <");
        assert_eq!(output.text.as_deref(), Some("hello "));
        let output = splitter.flush();
        assert_eq!(output.text.as_deref(), Some("<"));
    }

    #[test]
    fn stream_splitter_drops_truncated_dsml_marker() {
        let mut splitter = DsmlStreamSplitter::default();
        let output = splitter.push("<｜DS");
        assert!(output.text.is_none());
        let output = splitter.flush();
        assert!(output.text.is_none());
        assert!(output.tool_calls.is_empty());
    }

    #[test]
    fn stream_splitter_strips_stray_spaced_opener() {
        let input = format!("\n\n{}", open("｜", " calls"));
        let mut splitter = DsmlStreamSplitter::default();
        let mut text = String::new();
        let output = splitter.push(&input);
        if let Some(part) = output.text {
            text.push_str(&part);
        }
        let output = splitter.flush();
        if let Some(part) = output.text {
            text.push_str(&part);
        }
        assert!(!text.contains("｜DSML｜"), "text={text:?}");
    }

    #[test]
    fn reasoning_open_distinguishes_single_terminator_from_ellipsis() {
        assert!(!reasoning_is_open("enough."));
        assert!(!reasoning_is_open("好了。"));
        assert!(reasoning_is_open("thinking..."));
        assert!(reasoning_is_open("still going"));
    }

    #[test]
    fn continuation_start_is_conservative() {
        assert!(looks_like_reasoning_continuation_start("\n<DSML>`"));
        assert!(looks_like_reasoning_continuation_start(
            "_INVOKE_PREFIX = '"
        ));
        assert!(!looks_like_reasoning_continuation_start("当前结论如下"));
        assert!(!looks_like_reasoning_continuation_start(
            "Here is the answer"
        ));
    }

    #[test]
    fn strips_truncated_marker_that_never_completes() {
        let pipe = "｜";
        let input = format!("\n\n<{pipe}DSML{pipe} calls\n后续正常内容");
        let parsed = parse_dsml_tool_calls(&input);
        assert!(
            !parsed.content.contains("DSML"),
            "content={:?}",
            parsed.content
        );
        assert!(parsed.content.contains("后续正常内容"));
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn keeps_prose_with_a_lone_angle_bracket() {
        let parsed = parse_dsml_tool_calls("如果 a < b 且 b < c 就继续");
        assert_eq!(parsed.content, "如果 a < b 且 b < c 就继续");
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn stream_splitter_drops_truncated_marker_chunked_like_nim() {
        let pipe = "｜";
        let mut splitter = DsmlStreamSplitter::default();
        let mut text = String::new();
        for chunk in [
            format!("\n\n<{pipe}DSML{pipe}"),
            " calls".to_string(),
            "\n".to_string(),
        ] {
            let output = splitter.push(&chunk);
            if let Some(part) = output.text {
                text.push_str(&part);
            }
        }
        let output = splitter.flush();
        if let Some(part) = output.text {
            text.push_str(&part);
        }
        assert!(text.trim().is_empty(), "text={text:?}");
    }

    #[test]
    fn unterminated_block_keeps_real_text_and_drops_scaffolding() {
        let pipe = "｜";
        let block = format!(
            "<{pipe}DSML{pipe} calls>\n真实的回答内容\n<{pipe}DSML{pipe} invoke name=\"exec_command\">\n"
        );
        let salvaged = salvage_unterminated_block(&block);
        let text = salvaged.text.unwrap_or_default();
        assert!(text.contains("真实的回答内容"), "text={text:?}");
        assert!(!text.contains("invoke name"), "text={text:?}");
        assert!(!text.contains("DSML"), "text={text:?}");
    }

    #[test]
    fn unterminated_wrapper_still_converts_complete_invoke() {
        let pipe = "｜";
        let close = |body: &str| format!("{CLOSE_START}{pipe}DSML{pipe}{body}>");
        let close_invoke = close(" invoke");
        let block = format!(
            "<{pipe}DSML{pipe} calls>\n<{pipe}DSML{pipe} invoke name=\"exec_command\">\n<{pipe}DSML{pipe} parameter name=\"cmd\" string=\"true\">ls{}\n{}\n",
            close(" parameter"),
            close_invoke
        );
        let parsed = parse_dsml_tool_calls(&block);
        assert_eq!(parsed.tool_calls.len(), 1, "calls={:?}", parsed.tool_calls);
        assert_eq!(parsed.tool_calls[0].name, "exec_command");
        assert!(
            !parsed.content.contains("DSML"),
            "content={:?}",
            parsed.content
        );
        assert!(
            !parsed.content.contains("parameter"),
            "content={:?}",
            parsed.content
        );
    }
}
