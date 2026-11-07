//! Copilot can reissue an opaque item ID between Responses lifecycle events.
//! Keep the first upstream-issued ID for each output index so Codex can merge
//! streamed text with its completed item. The original ID remains replayable.

use crate::proxy::sse::{next_sse_block_range, scan_sse_block, strip_sse_field, Utf8TailDecoder};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::Value;
use std::{collections::HashMap, io};

/// `ItemIds::rewrite_event` 会读到的字段名：`output_index`、`item_id` 和
/// `response.output`。一个都不含的块（文本增量、推理增量、注释行……）哪怕解析出
/// 整棵 JSON 树也改不动任何字段，直接按原文重发。
const REWRITE_KEYS: [&str; 3] = ["\"output_index\"", "\"item_id\"", "\"output\""];

/// 原样放行一个块：块文本加一个空行，与逐字节重发完全一致。
fn passthrough_block(raw: &str) -> Bytes {
    let mut out = Vec::with_capacity(raw.len() + 2);
    out.extend_from_slice(raw.as_bytes());
    out.extend_from_slice(b"\n\n");
    Bytes::from(out)
}

#[derive(Default)]
struct ItemIds {
    by_output_index: HashMap<u64, String>,
}

impl ItemIds {
    fn stabilize_id(&mut self, output_index: u64, id: &mut Value) -> bool {
        let Some(current) = id.as_str().filter(|id| !id.is_empty()) else {
            return false;
        };
        let stable = self
            .by_output_index
            .entry(output_index)
            .or_insert_with(|| current.to_string());
        if current == stable {
            return false;
        }
        *id = Value::String(stable.clone());
        true
    }

    fn rewrite_event(&mut self, kind: &str, event: &mut Value) -> bool {
        if kind == "response.created" {
            self.by_output_index.clear();
        }

        let mut changed = false;
        if let Some(index) = event.get("output_index").and_then(Value::as_u64) {
            if matches!(
                kind,
                "response.output_item.added" | "response.output_item.done"
            ) {
                if let Some(id) = event.pointer_mut("/item/id") {
                    changed |= self.stabilize_id(index, id);
                }
            }
            if let Some(stable) = self.by_output_index.get(&index) {
                if let Some(Value::String(id)) = event.get_mut("item_id") {
                    if id != stable {
                        id.clone_from(stable);
                        changed = true;
                    }
                }
            }
        }

        if let Some(output) = event
            .pointer_mut("/response/output")
            .and_then(Value::as_array_mut)
        {
            for (index, item) in output.iter_mut().enumerate() {
                if let Some(id) = item.get_mut("id") {
                    changed |= self.stabilize_id(index as u64, id);
                }
            }
        }
        changed
    }

    fn rewrite_block(&mut self, raw: &str) -> Result<Bytes, io::Error> {
        if !REWRITE_KEYS.iter().any(|key| raw.contains(key)) {
            return Ok(passthrough_block(raw));
        }
        let (_, payload) = scan_sse_block(raw);
        let data = payload.as_deref().unwrap_or_default();
        let unchanged = || passthrough_block(raw);
        if data.is_empty() || data.trim() == "[DONE]" {
            return Ok(unchanged());
        }
        let mut event: Value = match serde_json::from_str(data) {
            Ok(event) => event,
            Err(error) => {
                log::warn!("[Copilot/Responses] Invalid SSE JSON: {error}");
                return Ok(unchanged());
            }
        };
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| raw.lines().find_map(|line| strip_sse_field(line, "event")))
            .unwrap_or_default()
            .to_string();
        if !self.rewrite_event(&kind, &mut event) {
            return Ok(unchanged());
        }

        let mut output = Vec::with_capacity(raw.len() + 16);
        let mut wrote_data = false;
        for line in raw.lines() {
            if strip_sse_field(line, "data").is_some() {
                if !wrote_data {
                    output.extend_from_slice(b"data: ");
                    serde_json::to_writer(&mut output, &event).map_err(io::Error::other)?;
                    output.push(b'\n');
                    wrote_data = true;
                }
            } else {
                output.extend_from_slice(line.as_bytes());
                output.push(b'\n');
            }
        }
        output.push(b'\n');
        Ok(Bytes::from(output))
    }
}

pub(crate) fn create_stable_item_id_stream<E>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send
where
    E: std::error::Error + Send + 'static,
{
    async_stream::try_stream! {
        let mut ids = ItemIds::default();
        let mut buffer = String::new();
        let mut utf8_tail = Utf8TailDecoder::default();

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|error| io::Error::other(error.to_string()))?;
            utf8_tail.append(&mut buffer, &bytes);
            while let Some((end, delim)) = next_sse_block_range(&buffer) {
                let block = &buffer[..end];
                let outcome = if block.trim().is_empty() {
                    None
                } else {
                    Some(ids.rewrite_block(block))
                };
                buffer.drain(..end + delim);
                if let Some(outcome) = outcome {
                    yield outcome?;
                }
            }
        }

        utf8_tail.flush(&mut buffer);
        let tail = std::mem::take(&mut buffer);
        if !tail.trim().is_empty() {
            yield ids.rewrite_block(&tail)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(event: &str, data: &str) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    async fn drain(blocks: Vec<String>) -> String {
        let stream = create_stable_item_id_stream(futures::stream::iter(
            blocks
                .into_iter()
                .map(|block| Ok::<_, io::Error>(Bytes::from(block))),
        ));
        futures::pin_mut!(stream);
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.extend_from_slice(&item.unwrap());
        }
        String::from_utf8(out).unwrap()
    }

    /// 按调用方给的原始分片走完整条流，返回拼接后的字节。
    async fn drain_chunks(chunks: &[&str]) -> Vec<u8> {
        let owned: Vec<String> = chunks.iter().map(|chunk| chunk.to_string()).collect();
        let stream = create_stable_item_id_stream(futures::stream::iter(
            owned
                .into_iter()
                .map(|chunk| Ok::<_, io::Error>(Bytes::from(chunk))),
        ));
        futures::pin_mut!(stream);
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.extend_from_slice(&item.unwrap());
        }
        out
    }

    /// 一个 chunk 里多个块：逐块发出，拼回来与原文一致。
    #[tokio::test]
    async fn stream_passes_many_blocks_in_one_chunk_through_byte_for_byte() {
        let chunk = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n: keep-alive\n\nevent: message\ndata: [DONE]\n\n";
        assert_eq!(drain_chunks(&[chunk]).await, chunk.as_bytes());
    }

    /// 事件被切断在两个 chunk 里：缓冲住，补齐后按原文发出。
    #[tokio::test]
    async fn stream_reassembles_an_event_split_across_chunks() {
        let head = "event: response.output_text.delta\ndata: {\"type\":";
        let tail = "\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        assert_eq!(
            drain_chunks(&[head, tail]).await,
            format!("{head}{tail}").as_bytes()
        );
    }

    /// 分隔符本身被切断：跨 chunk 的定界符记账不能错位。
    #[tokio::test]
    async fn stream_handles_a_delimiter_split_across_chunks() {
        let stream = "data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        assert_eq!(
            drain_chunks(&[&stream[..14], &stream[14..]]).await,
            stream.as_bytes()
        );
    }

    /// CRLF 分隔符：块文本原样、分隔符规范成 \n\n。
    #[tokio::test]
    async fn stream_normalizes_crlf_delimiters() {
        let chunk = "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\"}\r\n\r\n";
        assert_eq!(
            drain_chunks(&[chunk]).await,
            "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\"}\n\n"
                .as_bytes()
        );
    }

    /// id 改写跨过 chunk 边界照样成立。
    #[tokio::test]
    async fn stream_rewrites_ids_across_chunk_boundaries() {
        let added = sse(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#,
        );
        let done = sse(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_2"}}"#,
        );
        let text = String::from_utf8(
            drain_chunks(&[&added[..30], &added[30..], &done[..22], &done[22..]]).await,
        )
        .unwrap();
        let line = text
            .lines()
            .rfind(|line| line.starts_with("data: "))
            .unwrap();
        let event: Value = serde_json::from_str(&line["data: ".len()..]).unwrap();
        assert_eq!(event["item"]["id"], "rs_1");
    }

    /// Copilot 为同一个 output_index 重发 item id：拉回第一次的 id，Codex 才认得出
    /// 流式文本属于哪个条目。
    #[tokio::test]
    async fn rewrites_reissued_item_ids_for_the_same_output_index() {
        let stream = drain(vec![
            sse(
                "response.output_item.added",
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#,
            ),
            sse(
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","output_index":0,"item_id":"rs_1","delta":"hi"}"#,
            ),
            sse(
                "response.output_item.done",
                r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_2"}}"#,
            ),
        ])
        .await;
        let done = stream
            .lines()
            .rfind(|line| line.starts_with("data: "))
            .unwrap();
        let event: Value = serde_json::from_str(&done["data: ".len()..]).unwrap();
        assert_eq!(event["item"]["id"], "rs_1");
    }

    /// 不含任何可改字段的事件逐字节透传，event 行、注释、[DONE] 都不动。
    #[tokio::test]
    async fn passes_unrelated_events_through_byte_for_byte() {
        let blocks = vec![
            sse(
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","delta":"hi"}"#,
            ),
            "event: message\ndata: [DONE]\n\n".to_string(),
            ": keep-alive comment\n\n".to_string(),
        ];
        assert_eq!(drain(blocks.clone()).await, blocks.join(""));
    }

    /// `response.created` 带 output 数组：走完整路径但改不出东西，字节不变。
    #[tokio::test]
    async fn created_with_an_empty_output_is_untouched() {
        let block = sse(
            "response.created",
            r#"{"type":"response.created","response":{"id":"resp_1","output":[]}}"#,
        );
        assert_eq!(drain(vec![block.clone()]).await, block);
    }

    /// 载荷文本里出现 "item_id" 字面量：命中门控走完整路径，照样不改一个字节。
    #[tokio::test]
    async fn payload_mentioning_item_id_as_text_is_untouched() {
        let block = sse(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","delta":"键 \"item_id\" 只是文本"}"#,
        );
        assert_eq!(drain(vec![block.clone()]).await, block);
    }

    /// 载荷摊在多行 data 上：join 之后照旧判定，字节不变。
    #[tokio::test]
    async fn multi_line_payloads_are_untouched() {
        let raw = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\ndata: \"delta\":\"hi\"}\n\n";
        assert_eq!(drain(vec![raw.to_string()]).await, raw);
    }

    /// 上游最后一块没带空行：尾部路径同样处理，重发时补上分隔符。
    #[tokio::test]
    async fn handles_a_tail_without_a_delimiter() {
        let raw = "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\"}}";
        let mut expected = raw.to_string();
        expected.push_str("\n\n");
        assert_eq!(drain(vec![raw.to_string()]).await, expected);
    }
}
