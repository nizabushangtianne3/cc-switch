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
    next_sse_block_range_bytes, scan_sse_block, sse_block_payload, sse_data_type,
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

/// 会参与参数补齐的事件类型在 data 载荷里的原文（JSON 字符串值就是这种形式）。
/// 只保留能区分候选的最短前缀，命中之后仍由完整解析决定是不是真候选。
///
/// 块里一个都不含时，它的 type 不可能是候选：类型值一定以带引号的原文出现，
/// 与键顺序、行间空白、多行 data 都无关。于是这类块连 UTF-8 解码和 JSON 解析
/// 都不必做，直接在字节上放行。
const REPAIR_MARKERS: [&[u8]; 3] = [
    b"\"response.function_call_arguments.",
    b"\"response.output_item.done\"",
    b"\"response.completed\"",
];

/// 候选标记的字节搜索器。Finder 内部要预处理稀有字节表，构造一次不便宜，而
/// 透传流是每个请求一条、每个事件都要用，所以在开流时构造一份、整条流复用。
struct RepairMarkers {
    arguments: memchr::memmem::Finder<'static>,
    item_done: memchr::memmem::Finder<'static>,
    completed: memchr::memmem::Finder<'static>,
}

impl RepairMarkers {
    fn new() -> Self {
        Self {
            arguments: memchr::memmem::Finder::new(REPAIR_MARKERS[0]),
            item_done: memchr::memmem::Finder::new(REPAIR_MARKERS[1]),
            completed: memchr::memmem::Finder::new(REPAIR_MARKERS[2]),
        }
    }

    fn matches(&self, block: &[u8]) -> bool {
        self.arguments.find(block).is_some()
            || self.item_done.find(block).is_some()
            || self.completed.find(block).is_some()
    }
}

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

/// [`passthrough_block`] 的字节版：跨块或非 \n\n 分隔符的透传走这里。
fn passthrough_block_owned(raw: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(raw.len() + 2);
    out.extend_from_slice(raw);
    out.extend_from_slice(b"\n\n");
    Bytes::from(out)
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
        // 字节缓冲：UTF-8 的 0x0A/0x0D 只以自身出现，定界符扫描本来就不需要
        // 先解码整块；只有当块真的命中补齐逻辑时才解码那一个块。
        let mut buffer: Vec<u8> = Vec::new();
        let mut scratch = String::new();
        let mut repair = Repair::default();
        let markers = RepairMarkers::new();

        tokio::pin!(stream);
        let mut rewritten: Vec<u8> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(e) => {
                    repair.flush(None, &mut rewritten);
                    if !rewritten.is_empty() {
                        yield Ok(Bytes::copy_from_slice(&rewritten));
                        rewritten.clear();
                    }
                    yield Err(std::io::Error::other(e.to_string()));
                    return;
                }
            };
            let mut chunk_rest = buffer.len();
            buffer.extend_from_slice(&bytes);
            // 块整块来自本 chunk、且分隔符本就是 \n\n 时，直接切上游 chunk 的视图
            // 发出去，一次拷贝都不做。chunk_rest 记着缓冲前端还压着多少个上个
            // chunk 的尾巴字节：它为 0 时整块才确实来自本 chunk；chunk_cursor 记
            // 着本 chunk 已经发出多少字节，用于计算切片偏移。任何跨块、非 \n\n
            // 分隔符或命中补齐的块都会关掉切片，剩余部分退回拷贝。
            let mut chunk_cursor = 0usize;
            let mut sliceable = true;
            while let Some((end, delim)) = next_sse_block_range_bytes(&buffer) {
                let take = end + delim;
                let block = &buffer[..end];
                let leftover = chunk_rest;
                chunk_rest = leftover.saturating_sub(take);
                let from_chunk = take.saturating_sub(leftover);
                if block.trim_ascii().is_empty() {
                    buffer.drain(..take);
                    chunk_cursor += from_chunk;
                    continue;
                }
                if !markers.matches(block) {
                    if sliceable && leftover == 0 && delim == 2 {
                        // 与补齐无关的块：连同 \n\n 原样发出，字节与旧行为一致。
                        yield Ok(bytes.slice(chunk_cursor..chunk_cursor + take));
                    } else {
                        sliceable = false;
                        // 跨块或非 \n\n 分隔符：块文本加 \n\n，与旧行为一致。
                        yield Ok(passthrough_block_owned(block));
                    }
                    chunk_cursor += take;
                    buffer.drain(..take);
                    continue;
                }
                // 命中补齐逻辑的块很少：只解码这一个块，走原有解析与补齐。
                sliceable = false;
                scratch.clear();
                scratch.push_str(&String::from_utf8_lossy(block));
                repair.push(&scratch, &mut rewritten);
                if !rewritten.is_empty() {
                    yield Ok(Bytes::copy_from_slice(&rewritten));
                    rewritten.clear();
                }
                buffer.drain(..take);
            }
        }

        let tail = std::mem::take(&mut buffer);
        if !tail.trim_ascii().is_empty() {
            scratch = String::from_utf8_lossy(&tail).into_owned();
            repair.push(&scratch, &mut rewritten);
        }
        repair.flush(None, &mut rewritten);
        if !rewritten.is_empty() {
            yield Ok(Bytes::copy_from_slice(&rewritten));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::sse::next_sse_block_range;
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

    /// 走完整条流（字节进、字节出），按调用方给的分片原样喂进去。
    async fn run_stream(chunks: &[&str]) -> Vec<u8> {
        let owned: Vec<String> = chunks.iter().map(|chunk| chunk.to_string()).collect();
        let stream = create_late_arguments_repair_stream(futures::stream::iter(
            owned
                .into_iter()
                .map(|chunk| Ok::<_, std::io::Error>(Bytes::from(chunk))),
        ));
        futures::pin_mut!(stream);
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.extend_from_slice(&item.unwrap());
        }
        out
    }

    /// 一个 chunk 里塞满多个块：逐块零拷贝发出，拼回来与原文一致。
    #[tokio::test]
    async fn stream_passes_many_blocks_in_one_chunk_through_byte_for_byte() {
        let chunk = "event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n: keep-alive\n\n";
        assert_eq!(run_stream(&[chunk]).await, chunk.as_bytes());
    }

    /// 事件被切断在两个 chunk 里：缓冲住，补齐后按原文发出。
    #[tokio::test]
    async fn stream_reassembles_an_event_split_across_chunks() {
        let head = "event: response.output_text.delta\ndata: {\"type\":";
        let tail = "\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        assert_eq!(
            run_stream(&[head, tail]).await,
            format!("{head}{tail}").as_bytes()
        );
    }

    /// 分隔符本身被切断（首个 \n 在一个 chunk，第二个 \n 在下一个）。
    #[tokio::test]
    async fn stream_handles_a_delimiter_split_across_chunks() {
        let stream = "data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        assert_eq!(
            run_stream(&[&stream[..14], &stream[14..]]).await,
            stream.as_bytes()
        );
    }

    /// CRLF 分隔符走拷贝路径，仍旧规范成 \n\n：与旧行为逐字节一致。
    #[tokio::test]
    async fn stream_normalizes_crlf_delimiters() {
        let chunk = "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\"}\r\n\r\n";
        assert_eq!(
            run_stream(&[chunk]).await,
            "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\"}\n\n"
                .as_bytes()
        );
    }

    /// 两个定界符贴在一起产生的空块被丢弃，不发给客户端。
    #[tokio::test]
    async fn stream_drops_empty_blocks() {
        assert_eq!(
            run_stream(&["data: {\"a\":1}\n\n\n\ndata: {\"b\":2}\n\n"]).await,
            "data: {\"a\":1}\n\ndata: {\"b\":2}\n\n".as_bytes()
        );
    }

    /// 最后一个块没带分隔符：流结束时补上 \n\n 发出。
    #[tokio::test]
    async fn stream_flushes_a_tail_without_a_delimiter() {
        let tail = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}";
        assert_eq!(run_stream(&[tail]).await, format!("{tail}\n\n").as_bytes());
    }

    /// MiniMax 乱序场景跨过多个诡异分片：扣住、补参数、按序发出仍然成立。
    #[tokio::test]
    async fn stream_repairs_late_arguments_across_awkward_chunks() {
        let done = "event: response.function_call_arguments.done\ndata: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"x_fc_0\",\"arguments\":\"\"}\n\n";
        let item_done = "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"x_fc_0\",\"arguments\":\"\"}}\n\n";
        let delta = "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"x_fc_0\",\"delta\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}\n\n";
        let completed = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n";
        let text = String::from_utf8(
            run_stream(&[
                &done[..40],
                &done[40..],
                &item_done[..17],
                &item_done[17..],
                &delta[..25],
                &delta[25..],
                completed,
            ])
            .await,
        )
        .unwrap();
        let events: Vec<Value> = text
            .split("\n\n")
            .filter(|block| !block.is_empty())
            .map(|block| {
                let data = block
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .unwrap();
                serde_json::from_str(data).unwrap()
            })
            .collect();
        assert_eq!(
            types(&events),
            vec![
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_eq!(events[1]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(events[2]["item"]["arguments"], "{\"cmd\":\"ls\"}");
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
