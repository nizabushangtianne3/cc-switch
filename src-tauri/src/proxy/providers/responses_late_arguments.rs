//! 原生 Responses 透传（官方以外的上游）：补齐迟到的函数调用参数。
//!
//! MiniMax 的 `/v1/responses` 在长历史下（整段参数一次吐出时）会乱序：先发
//! `function_call_arguments.done` 和 `output_item.done`，`arguments` 都是空串，再发唯一一个
//! 带完整参数的 `function_call_arguments.delta`，最后 `response.completed` 里的参数是对的
//! （实测 2026-10-03）。Codex 在 `output_item.done` 就定下调用，拿到空串，工具全部报
//! `failed to parse function arguments`。
//!
//! 这里只动响应：参数为空的那两个结束事件先扣住，等之后第一个不是参数增量的事件到来时，
//! 用累积的增量（或 `response.completed` 里的同 id 条目）补上参数再发。顺序正常的流
//! 里结束事件本来就带参数，原样放行；真没有参数的调用，扣住的事件原样补发。请求一个字节不改。

use std::collections::HashMap;

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::Value;

use crate::proxy::sse::{
    next_sse_block_range, scan_sse_block, sse_block_payload, sse_data_type, Utf8TailDecoder,
};

/// 会参与参数补齐的事件类型。
///
/// 其余事件（文本/推理增量、生命周期事件、注释行……）只是原样透传，没有必要
/// 为它们解析整棵 JSON 树、复制块文本、再造一个 `event:` 名字。
const REPAIR_EVENT_TYPES: [&str; 4] = [
    "response.function_call_arguments.delta",
    "response.function_call_arguments.done",
    "response.output_item.done",
    "response.completed",
];

/// 一个 SSE 块，解析出的事件（不是 JSON 的块为 None）。
struct Block {
    raw: String,
    event_name: Option<String>,
    event: Option<Value>,
}

impl Block {
    fn parse(raw: &str) -> Self {
        let (event_name, payload) = scan_sse_block(raw);
        let event = payload
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        Self {
            raw: raw.to_string(),
            event_name,
            event,
        }
    }

    fn event_type(&self) -> Option<&str> {
        self.event.as_ref()?.get("type")?.as_str()
    }

    /// 参数为空的函数调用结束事件，返回条目 id。
    fn empty_function_call_end(&self) -> Option<String> {
        let event = self.event.as_ref()?;
        let (item_id, arguments) = match self.event_type()? {
            "response.function_call_arguments.done" => {
                (event.get("item_id")?, event.get("arguments"))
            }
            "response.output_item.done" => {
                let item = event.get("item")?;
                if item.get("type")?.as_str()? != "function_call" {
                    return None;
                }
                (item.get("id")?, item.get("arguments"))
            }
            _ => return None,
        };
        let empty = arguments
            .and_then(Value::as_str)
            .is_none_or(|arguments| arguments.is_empty());
        empty
            .then(|| item_id.as_str().map(str::to_string))
            .flatten()
    }

    fn arguments_delta(&self) -> Option<(String, &str)> {
        if self.event_type()? != "response.function_call_arguments.delta" {
            return None;
        }
        let event = self.event.as_ref()?;
        Some((
            event.get("item_id")?.as_str()?.to_string(),
            event.get("delta")?.as_str()?,
        ))
    }

    /// 把参数写进结束事件，重新序列化。
    fn with_arguments(mut self, arguments: &str, out: &mut Vec<u8>) {
        let Some(event) = self.event.as_mut() else {
            return self.into_bytes(out);
        };
        let target =
            if event.get("type").and_then(Value::as_str) == Some("response.output_item.done") {
                event.get_mut("item")
            } else {
                Some(&mut *event)
            };
        let Some(target) = target.and_then(Value::as_object_mut) else {
            return self.into_bytes(out);
        };
        target.insert(
            "arguments".to_string(),
            Value::String(arguments.to_string()),
        );
        // 与 frame_sse_event 一致：直接写进调用方的块缓冲，不留中间 String。
        if let Some(name) = &self.event_name {
            out.extend_from_slice(b"event: ");
            out.extend_from_slice(name.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(b"data: ");
        let _ = serde_json::to_writer(&mut *out, event);
        out.extend_from_slice(b"\n\n");
    }

    fn into_bytes(self, out: &mut Vec<u8>) {
        passthrough_block(out, &self.raw);
    }
}

/// 原样放行一个块：块文本加一个空行，与逐字节重发完全一致。
fn passthrough_block(out: &mut Vec<u8>, raw: &str) {
    out.extend_from_slice(raw.as_bytes());
    out.extend_from_slice(b"\n\n");
}

#[derive(Default)]
struct Repair {
    /// 每个条目累积的参数增量。
    arguments: HashMap<String, String>,
    /// 扣住的结束事件，保持原来的顺序。
    held: Vec<(String, Block)>,
}

impl Repair {
    /// 处理一个块，输出追加进 `out`：同一个 chunk 的若干块合成一次 yield，
    /// 不再一个块一次分配、一次流式写。
    fn push(&mut self, raw: &str, out: &mut Vec<u8>) {
        // 与补齐无关的事件才是流的绝大多数：借用的 data 载荷加一个借用的 type
        // 探针就够判定，不解析 JSON、不复制块文本。没有 data 行、载荷摊在多行、
        // 或者类型命中候选的事件，才走下面的完整解析。
        let repair_candidate = sse_block_payload(raw)
            .and_then(sse_data_type)
            .is_some_and(|event_type| REPAIR_EVENT_TYPES.contains(&event_type));
        if !repair_candidate {
            // 语义与完整路径一致：任何「不是参数增量」的事件都会放行此前扣住的
            // 结束事件，然后自己原样发出。
            self.flush(None, out);
            passthrough_block(out, raw);
            return;
        }

        let block = Block::parse(raw);
        if let Some((item_id, delta)) = block.arguments_delta() {
            self.arguments.entry(item_id).or_default().push_str(delta);
            // 增量一律照发，不触发放行：并行调用时几个条目的结束事件可能都先到，各自的增量
            // （乃至别的调用的增量）随后交错着来，见到任何增量就放行会让还没等到参数的条目空着发出。
            block.into_bytes(out);
            return;
        }

        if let Some(item_id) = block.empty_function_call_end() {
            self.held.push((item_id, block));
            return;
        }

        let completed = block
            .event
            .as_ref()
            .filter(|_| block.event_type() == Some("response.completed"));
        self.flush(completed, out);
        block.into_bytes(out);
    }

    fn flush(&mut self, completed: Option<&Value>, out: &mut Vec<u8>) {
        std::mem::take(&mut self.held)
            .into_iter()
            .for_each(|(item_id, block)| {
                let arguments = self
                    .arguments
                    .get(&item_id)
                    .filter(|arguments| !arguments.is_empty())
                    .cloned()
                    .or_else(|| completed_arguments(completed?, &item_id));
                match arguments {
                    Some(arguments) => block.with_arguments(&arguments, out),
                    None => block.into_bytes(out),
                }
            });
    }
}

/// `response.completed` 里同 id 函数调用的参数。
fn completed_arguments(completed: &Value, item_id: &str) -> Option<String> {
    completed
        .pointer("/response/output")?
        .as_array()?
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(item_id))?
        .get("arguments")?
        .as_str()
        .filter(|arguments| !arguments.is_empty())
        .map(str::to_string)
}

/// 包一层原生 Responses SSE 流，补齐迟到的函数调用参数。
pub(crate) fn create_late_arguments_repair_stream<E>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + 'static,
{
    async_stream::stream! {
        let mut buffer = String::new();
        let mut utf8_tail = Utf8TailDecoder::default();
        let mut repair = Repair::default();

        tokio::pin!(stream);
        let mut out: Vec<u8> = Vec::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    utf8_tail.append(&mut buffer, &bytes);
                    while let Some((end, delim)) = next_sse_block_range(&buffer) {
                        let block = &buffer[..end];
                        if !block.trim().is_empty() {
                            repair.push(block, &mut out);
                        }
                        buffer.drain(..end + delim);
                    }
                    // 一个 chunk 的所有块合成一次 yield：下游看到的字节序列不变，
                    // 但省下每块一次分配和一次流式写。
                    if !out.is_empty() {
                        yield Ok(Bytes::copy_from_slice(&out));
                        out.clear();
                    }
                }
                Err(e) => {
                    repair.flush(None, &mut out);
                    if !out.is_empty() {
                        yield Ok(Bytes::copy_from_slice(&out));
                        out.clear();
                    }
                    yield Err(std::io::Error::other(e.to_string()));
                    return;
                }
            }
        }

        utf8_tail.flush(&mut buffer);
        let tail = std::mem::take(&mut buffer);
        if !tail.trim().is_empty() {
            repair.push(&tail, &mut out);
        }
        repair.flush(None, &mut out);
        if !out.is_empty() {
            yield Ok(Bytes::copy_from_slice(&out));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sse(event: Value) -> String {
        format!(
            "event: {}\ndata: {}",
            event["type"].as_str().unwrap(),
            event
        )
    }

    fn run(blocks: Vec<Value>) -> Vec<Value> {
        let mut repair = Repair::default();
        let mut stream: Vec<u8> = Vec::new();
        for event in blocks {
            repair.push(&sse(event), &mut stream);
        }
        repair.flush(None, &mut stream);
        let text = String::from_utf8(stream).unwrap();
        let mut events = Vec::new();
        let mut rest = text.as_str();
        while let Some((end, delim)) = next_sse_block_range(rest) {
            let data = rest[..end]
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            events.push(serde_json::from_str(data).unwrap());
            rest = &rest[end + delim..];
        }
        events
    }

    fn types(events: &[Value]) -> Vec<&str> {
        events.iter().map(|e| e["type"].as_str().unwrap()).collect()
    }

    fn call_item(arguments: &str) -> Value {
        json!({ "type": "function_call", "id": "x_fc_0", "call_id": "call_1",
                "name": "exec_command", "arguments": arguments })
    }

    /// 实测的 MiniMax 乱序：结束事件先到、参数为空，唯一的增量排在后面。
    #[test]
    fn fills_arguments_that_arrive_after_the_end_events() {
        let events = run(vec![
            json!({ "type": "response.output_item.added", "output_index": 3, "item": call_item("") }),
            json!({ "type": "response.function_call_arguments.done", "output_index": 3,
                    "item_id": "x_fc_0", "name": "exec_command", "arguments": "" }),
            json!({ "type": "response.output_item.done", "output_index": 3, "item": call_item("") }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 3,
                    "item_id": "x_fc_0", "delta": "{\"cmd\":\"ls\"}" }),
            json!({ "type": "response.completed", "response": { "output": [call_item("{\"cmd\":\"ls\"}")] } }),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_eq!(events[2]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(events[3]["item"]["arguments"], "{\"cmd\":\"ls\"}");
    }

    /// 并行调用：两个条目的结束事件都先到，各自的增量随后才来，两个都要补上。
    #[test]
    fn fills_parallel_calls_whose_arguments_arrive_after_all_end_events() {
        let item = |id: &str, arguments: &str| {
            json!({ "type": "function_call", "id": id, "call_id": format!("call_{id}"),
                    "name": "exec_command", "arguments": arguments })
        };
        let events = run(vec![
            json!({ "type": "response.output_item.done", "output_index": 0, "item": item("a", "") }),
            json!({ "type": "response.output_item.done", "output_index": 1, "item": item("b", "") }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 0,
                    "item_id": "a", "delta": "{\"cmd\":\"ls\"}" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 1,
                    "item_id": "b", "delta": "{\"cmd\":\"pwd\"}" }),
            json!({ "type": "response.completed", "response": { "output": [] } }),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.output_item.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_eq!(events[2]["item"]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(events[3]["item"]["arguments"], "{\"cmd\":\"pwd\"}");
    }

    /// 别的调用的增量插在中间（A → C → B）：扣住的 A、B 不能被 C 的增量提前放走。
    #[test]
    fn unrelated_deltas_do_not_release_held_calls() {
        let item = |id: &str| {
            json!({ "type": "function_call", "id": id, "call_id": format!("call_{id}"),
                    "name": "exec_command", "arguments": "" })
        };
        let delta = |id: &str, delta: &str| json!({ "type": "response.function_call_arguments.delta", "item_id": id, "delta": delta });
        let events = run(vec![
            json!({ "type": "response.output_item.done", "output_index": 0, "item": item("a") }),
            json!({ "type": "response.output_item.done", "output_index": 1, "item": item("b") }),
            delta("a", "{\"cmd\":\"ls\"}"),
            delta("c", "{\"cmd\":"),
            delta("b", "{\"cmd\":\"pwd\"}"),
            json!({ "type": "response.completed", "response": { "output": [] } }),
        ]);
        let done: Vec<(&str, &str)> = events
            .iter()
            .filter(|e| e["type"] == "response.output_item.done")
            .map(|e| {
                (
                    e["item"]["id"].as_str().unwrap(),
                    e["item"]["arguments"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            done,
            vec![("a", "{\"cmd\":\"ls\"}"), ("b", "{\"cmd\":\"pwd\"}")]
        );
    }

    /// 没有增量时，从 `response.completed` 里同 id 的条目取参数。
    #[test]
    fn falls_back_to_the_completed_output() {
        let events = run(vec![
            json!({ "type": "response.output_item.done", "output_index": 0, "item": call_item("") }),
            json!({ "type": "response.completed", "response": { "output": [call_item("{\"a\":1}")] } }),
        ]);
        assert_eq!(events[0]["item"]["arguments"], "{\"a\":1}");
        assert_eq!(events[1]["type"], "response.completed");
    }

    /// 顺序正常的流原样放行，字节不变。
    #[test]
    fn leaves_well_ordered_streams_untouched() {
        let blocks = vec![
            json!({ "type": "response.function_call_arguments.delta", "item_id": "x_fc_0", "delta": "{\"cmd\":" }),
            json!({ "type": "response.function_call_arguments.delta", "item_id": "x_fc_0", "delta": "\"ls\"}" }),
            json!({ "type": "response.function_call_arguments.done", "item_id": "x_fc_0", "arguments": "{\"cmd\":\"ls\"}" }),
            json!({ "type": "response.output_item.done", "item": call_item("{\"cmd\":\"ls\"}") }),
            json!({ "type": "response.completed", "response": { "output": [] } }),
        ];
        let mut repair = Repair::default();
        let mut out: Vec<u8> = Vec::new();
        for event in blocks {
            let raw = sse(event);
            repair.push(&raw, &mut out);
            assert_eq!(out, format!("{raw}\n\n").as_bytes());
            out.clear();
        }
    }

    /// 与参数补齐无关的事件：逐字节原样发出，一个字节都不改。
    #[test]
    fn passes_unrelated_events_through_byte_for_byte() {
        let unrelated = [
            "event: response.created\ndata: {\"type\":\"response.created\"}",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}",
            "event: message\ndata: [DONE]",
            ": keep-alive comment",
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\"}",
        ];
        let mut repair = Repair::default();
        let mut out: Vec<u8> = Vec::new();
        for raw in unrelated {
            repair.push(raw, &mut out);
            assert_eq!(out, format!("{raw}\n\n").as_bytes(), "{raw}");
            out.clear();
        }
    }

    /// 载荷按 SSE 规范摊在多行上：慢路径照旧接管，字节同样不变。
    #[test]
    fn multi_line_payloads_are_also_untouched() {
        let raw = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\ndata: \"delta\":\"hi\"}";
        let mut repair = Repair::default();
        let mut out: Vec<u8> = Vec::new();
        repair.push(raw, &mut out);
        assert_eq!(out, format!("{raw}\n\n").as_bytes());
    }

    /// 没有 data 行的块（纯注释/纯 event）也走快路径，但扣住的结束事件必须放行。
    #[test]
    fn data_less_blocks_release_held_events() {
        let mut repair = Repair::default();
        let mut out: Vec<u8> = Vec::new();
        repair.push(
            &sse(json!({
                "type": "response.output_item.done",
                "item": call_item("")
            })),
            &mut out,
        );
        assert!(out.is_empty(), "held block must not be emitted yet");
        repair.push("event: ping", &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "event: response.output_item.done\ndata: {}\n\nevent: ping\n\n",
                json!({ "type": "response.output_item.done", "item": call_item("") })
            )
        );
    }

    /// 真没有参数的调用：扣住的事件在下一个事件前原样补发，不丢。
    #[test]
    fn releases_held_events_without_arguments_unchanged() {
        let events = run(vec![
            json!({ "type": "response.output_item.done", "output_index": 0, "item": call_item("") }),
            json!({ "type": "response.output_item.added", "output_index": 1,
                    "item": { "type": "message", "id": "m1" } }),
        ]);
        assert_eq!(
            types(&events),
            vec!["response.output_item.done", "response.output_item.added"]
        );
        assert_eq!(events[0]["item"]["arguments"], "");

        // 流在扣住时结束，也照样补发。
        let events = run(vec![
            json!({ "type": "response.output_item.done", "item": call_item("") }),
        ]);
        assert_eq!(events.len(), 1);
    }
}
