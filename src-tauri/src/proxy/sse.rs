use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use smallvec::SmallVec;
use std::borrow::Cow;

#[inline]
pub(crate) fn strip_sse_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    // 逐字段零分配：SSE 每行都要过这里，format! 的两次临时分配
    // 在长流式响应里是数千次小分配。语义与 "field: value" / "field:value"
    // 两种前缀完全一致。
    let rest = line.strip_prefix(field)?;
    let rest = rest.strip_prefix(':')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// 一个 SSE 块里的 `data:` 载荷，直接借用块文本。
///
/// 没有 data 行、或载荷按 SSE 规范摊在多行上时返回 `None`：调用方据此走自己的
/// 慢路径（那边才会拼出 owned 的连接结果）。Responses / Chat Completions /
/// Anthropic 的载荷都是单行，这才是每事件都要过的路，因此这里既不分配，也不
/// 顺手去构造用不上的 `event:` 名字。
pub(crate) fn sse_block_payload(block: &str) -> Option<&str> {
    let mut payload: Option<&str> = None;
    for line in block.lines() {
        if let Some(data) = strip_sse_field(line, "data") {
            if payload.is_some() {
                return None;
            }
            payload = Some(data);
        }
    }
    payload
}

/// 只读顶层 `type` 的探针：借用反序列化，其余字段由 serde 跳过。
#[derive(Deserialize)]
struct SseTypeProbe<'a> {
    #[serde(rename = "type", borrow)]
    r#type: Option<&'a str>,
}

/// 取 SSE 载荷的顶层 `type`，不建整棵 JSON `Value`。
///
/// 只为判断事件类型的调用方准备：解析失败（非 JSON，或 `type` 不是字符串）时
/// 返回 `None`，与解析成 `Value` 后取不到字符串类型完全一致。
pub(crate) fn sse_data_type(data: &str) -> Option<&str> {
    serde_json::from_str::<SseTypeProbe<'_>>(data).ok()?.r#type
}

#[inline]
pub(crate) fn next_sse_block_range(buffer: &str) -> Option<(usize, usize)> {
    next_sse_block_range_bytes(buffer.as_bytes())
}

/// [`next_sse_block_range`] 的字节版：透传流在字节缓冲上扫描，不必先把整块
/// 解码成 `String`。
///
/// UTF-8 的 0x0A/0x0D 只可能以自身出现（多字节序列的每个字节都 >= 0x80），
/// 所以字节扫描与字符串扫描的定界符判定完全一致；跨块被切断的多字节字符只是
/// 留在缓冲里，等到块完整再一次性解码。
#[inline]
pub(crate) fn next_sse_block_range_bytes(buffer: &[u8]) -> Option<(usize, usize)> {
    let bytes = buffer;
    // Scan once instead of running four searches over the remaining stream.
    // The delimiters all start with CR or LF, so the byte search can jump
    // straight to each candidate instead of testing every byte of the event
    // text; `memchr2` is SIMD-accelerated, which matters because this is the
    // single hottest loop of the streaming response path.
    //
    // The first CR/LF that actually starts a delimiter is the one that must be
    // consumed, even when another delimiter follows immediately. A lone CR is
    // not a delimiter on its own, so it is skipped and the search resumes from
    // the next byte.
    let mut cursor = 0;
    while cursor < bytes.len() {
        let index = match memchr::memchr2(b'\r', b'\n', &bytes[cursor..]) {
            Some(offset) => cursor + offset,
            None => return None,
        };
        let delimiter_len = match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                match (bytes.get(index + 2), bytes.get(index + 3)) {
                    (Some(b'\r'), Some(b'\n')) => 4,
                    (Some(b'\n'), _) => 3,
                    _ => 0,
                }
            }
            b'\n' => match (bytes.get(index + 1), bytes.get(index + 2)) {
                (Some(b'\r'), Some(b'\n')) => 3,
                (Some(b'\n'), _) => 2,
                _ => 0,
            },
            _ => 0,
        };
        if delimiter_len > 0 {
            return Some((index, delimiter_len));
        }
        cursor = index + 1;
    }
    None
}

/// A character is split across chunk boundaries at most once, and UTF-8 needs
/// at most 4 bytes, so the tail fits in an inline array. Keeping it out of the
/// heap is the point: a Chinese SSE stream cuts a 3-byte character at a token
/// boundary on many chunks, and a `Vec` there costs one allocation plus a copy
/// of the whole chunk every single time.
#[derive(Default)]
pub(crate) struct Utf8TailDecoder {
    tail: [u8; 4],
    len: u8,
}

impl Utf8TailDecoder {
    /// Append raw bytes to a UTF-8 `String` buffer, correctly handling multi-byte
    /// characters that are split across chunks.
    ///
    /// Trailing incomplete bytes are stashed inline and prepended to the next
    /// chunk's head, so the join never allocates. Bytes that cannot start or
    /// continue a valid sequence emit one U+FFFD each, matching
    /// `String::from_utf8_lossy` on the concatenated input.
    pub(crate) fn append(&mut self, buffer: &mut String, new_bytes: &[u8]) {
        let Some(first) = self.tail[0..self.len as usize].first().copied() else {
            self.decode_whole_chunk(buffer, new_bytes);
            return;
        };
        self.decode_from_tail(buffer, new_bytes, utf8_char_len(first));
    }

    /// Whole chunk is self-contained: the common case, and it never allocates.
    fn decode_whole_chunk(&mut self, buffer: &mut String, bytes: &[u8]) {
        let mut rest = bytes;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    buffer.push_str(text);
                    return;
                }
                Err(err) => {
                    // `valid_up_to` is verified UTF-8 by definition, so the lossy
                    // fallback borrows and never allocates on this path.
                    buffer.push_str(&String::from_utf8_lossy(&rest[..err.valid_up_to()]));
                    match err.error_len() {
                        Some(invalid) => {
                            buffer.push('\u{FFFD}');
                            rest = &rest[err.valid_up_to() + invalid..];
                        }
                        // Incomplete trailing sequence: stash it for the next chunk.
                        None => {
                            let tail = &rest[err.valid_up_to()..];
                            self.tail[..tail.len()].copy_from_slice(tail);
                            self.len = tail.len() as u8;
                            return;
                        }
                    }
                }
            }
        }
    }

    /// A character is pending from the previous chunk: only the bytes that
    /// finish it are needed, so the join happens on the stack.
    fn decode_from_tail(&mut self, buffer: &mut String, bytes: &[u8], expected: usize) {
        let mut joined = [0u8; 4];
        let carried = self.len as usize;
        joined[..carried].copy_from_slice(&self.tail[..carried]);
        // 待定字符此刻已经搬进 joined，tail 必须在此刻清空：下面的错误路径会带着
        // joined 的剩余字节递归回 append，若不先清掉，同一个前导字节会被反复重拼，
        // 直到把栈耗尽。
        self.len = 0;

        let missing = expected - carried;
        let taken = missing.min(bytes.len());
        joined[carried..carried + taken].copy_from_slice(&bytes[..taken]);

        if taken < missing {
            // Still not enough bytes to finish the character.
            self.tail[..carried + taken].copy_from_slice(&joined[..carried + taken]);
            self.len = (carried + taken) as u8;
            return;
        }

        match std::str::from_utf8(&joined[..expected]) {
            Ok(text) => buffer.push_str(text),
            Err(err) => {
                buffer.push_str(&String::from_utf8_lossy(&joined[..err.valid_up_to()]));
                buffer.push('\u{FFFD}');
                let after = err.valid_up_to()
                    + err
                        .error_len()
                        .expect("a lead byte plus its continuation bytes cannot be incomplete");
                self.append(buffer, &joined[after..expected]);
                // 不能在这里返回：joined 消耗掉的是本 chunk 开头的 taken 个字节，
                // bytes[taken..] 还没解码。解码器是个状态机，先落 joined 的剩余
                // 字节（可能又攒出一个新尾巴）再接着落 chunk 尾部，顺序才正确。
            }
        }
        self.append(buffer, &bytes[taken..]);
    }

    /// Flush a tail that is still pending when the upstream stream ends.
    ///
    /// A truncated character can never be completed by a chunk that never
    /// arrives, so the callers that keep reading events out of the buffer past
    /// the end of the stream emit it lossily instead of dropping it.
    pub(crate) fn flush(&mut self, buffer: &mut String) {
        let pending = &self.tail[..self.len as usize];
        if pending.is_empty() {
            return;
        }
        buffer.push_str(&String::from_utf8_lossy(pending));
        self.len = 0;
    }
}

/// Frame one event as SSE bytes: `event: <name>\ndata: <json>\n\n`.
///
/// Writing straight into a single buffer avoids the two extra copies the
/// callers used to pay: `serde_json::to_string` produced an intermediate String,
/// `format!` copied it into a second String, and `Bytes::from` copied that into
/// the chunk. `Bytes::from(Vec)` takes ownership, so a emitted event now costs
/// one allocation and one write.
///
/// The payload is a `serde_json::Value`, which always serialises; a write error
/// therefore cannot occur, and the prior `unwrap_or_default()` behaviour (an
/// empty `data:` line) is preserved for the impossible case.
pub(crate) fn frame_sse_event(event_name: &str, payload: &Value) -> Bytes {
    let mut frame = Vec::with_capacity(event_name.len() + 64);
    frame.extend_from_slice(b"event: ");
    frame.extend_from_slice(event_name.as_bytes());
    frame.extend_from_slice(b"\ndata: ");
    let _ = serde_json::to_writer(&mut frame, payload);
    frame.extend_from_slice(b"\n\n");
    Bytes::from(frame)
}

/// `content_block_delta` 的直接字节写出。
///
/// 增量事件占各协议转换层输出的绝大多数，`json!` 每发一个都要为一棵树分配十来
/// 个 String、再整棵序列化；这里按固定键序直接写字节，字符串值交给 serde 的写
/// 入器转义（与整棵序列化同一套规则），输出与 `json!` + [`frame_sse_event`]
/// 逐字节一致。`delta_type` 取值 `text_delta` / `thinking_delta` /
/// `signature_delta` / `input_json_delta`，`field` 是它对应的载荷字段名。
pub(crate) fn content_block_delta_sse(
    index: u32,
    delta_type: &str,
    field: &str,
    value: &str,
) -> Bytes {
    let mut frame = Vec::with_capacity(value.len() + 96);
    frame.extend_from_slice(
        b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":",
    );
    let _ = serde_json::to_writer(&mut frame, &index);
    frame.extend_from_slice(b",\"delta\":{\"type\":\"");
    frame.extend_from_slice(delta_type.as_bytes());
    frame.extend_from_slice(b"\",\"");
    frame.extend_from_slice(field.as_bytes());
    frame.extend_from_slice(b"\":");
    let _ = serde_json::to_writer(&mut frame, value);
    frame.extend_from_slice(b"}}\n\n");
    Bytes::from(frame)
}

/// Split one SSE block into its `event:` value and its `data:` payload.
///
/// The data lines are borrowed rather than copied. Responses, Chat Completions
/// and Anthropic events all carry their payload on a single `data:` line, so
/// the common path allocates nothing for the payload; the multi-line case that
/// the SSE spec also allows still joins with newlines exactly as before.
///
/// 拆出一个 SSE 块的 `event:` 名与 `data:` 载荷，事件名借用块文本。
///
/// 事件名曾经是 `String`：每个块一次堆分配，长流式响应几百个块就是几百次，
/// 而名字只用来和几个字面量比一比。现在统一借用，需要 owned 的调用方自己
/// `.map(str::to_string)`——全仓只有 `responses_late_arguments` 那一个。
///
/// 借用约束：使用期间不能动块所在的缓冲。所有调用点的块循环都是「推进游标、
/// 整 chunk 结束时才 drain 一次」（见 `streaming_responses` / `streaming_codex_chat` /
/// `streaming_codex_anthropic` / xAI 整流），块处理期间借用一直有效。
///
/// 同一块里重复的 `event:` 后者胜出，与旧的内联循环一致。
pub(crate) fn scan_sse_block(block: &str) -> (Option<&'_ str>, Option<Cow<'_, str>>) {
    let mut event: Option<&str> = None;

    for line in block.lines() {
        if let Some(name) = strip_sse_field(line, "event") {
            event = Some(name.trim());
        }
    }

    (event, sse_block_data(block))
}

/// 只取一个 SSE 块的 `data:` 载荷，连 `event:` 名字都不看。
///
/// 调用方不用事件名时走这里：`scan_sse_block` 每个块都要为那个名字分配一个
/// 名字，而 data 载荷本身是借用的、一个字节都不复制。单行 data 照旧零分配，
/// 多行 data 用换行拼一份，语义与 `scan_sse_block` 完全一致。
pub(crate) fn sse_block_data(block: &str) -> Option<Cow<'_, str>> {
    let mut data: SmallVec<[&str; 2]> = SmallVec::new();
    for line in block.lines() {
        if let Some(payload) = strip_sse_field(line, "data") {
            data.push(payload);
        }
    }

    match data.as_slice() {
        [] => None,
        [single] => Some(Cow::Borrowed(*single)),
        many => {
            let mut joined = String::new();
            for (index, part) in many.iter().enumerate() {
                if index > 0 {
                    joined.push('\n');
                }
                joined.push_str(part);
            }
            Some(Cow::Owned(joined))
        }
    }
}

/// Byte count of the UTF-8 character introduced by `lead`.
#[inline]
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
/// SSE events of a body that is already complete, borrowed in place.
///
/// [`Self::remainder`] is the trailing text after the last delimiter, which
/// callers treat as a final event when the upstream omitted the blank line.
pub(crate) struct SseBlocks<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> SseBlocks<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    pub(crate) fn remainder(&self) -> &'a str {
        &self.text[self.pos..]
    }
}

impl<'a> Iterator for SseBlocks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let rest = &self.text[self.pos..];
        let (end, delim) = next_sse_block_range(rest)?;
        self.pos += end + delim;
        Some(&rest[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::scan_sse_block;
    use super::{
        frame_sse_event, next_sse_block_range, sse_block_data, sse_block_payload, sse_data_type,
        strip_sse_field, SseBlocks, Utf8TailDecoder,
    };
    use bytes::Bytes;
    use std::borrow::Cow;

    #[test]
    fn strip_sse_field_accepts_optional_space() {
        assert_eq!(
            strip_sse_field("data: {\"ok\":true}", "data"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            strip_sse_field("data:{\"ok\":true}", "data"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            strip_sse_field("event: message_start", "event"),
            Some("message_start")
        );
        assert_eq!(
            strip_sse_field("event:message_start", "event"),
            Some("message_start")
        );
        assert_eq!(strip_sse_field("id:1", "data"), None);
    }

    #[test]
    fn next_sse_block_range_supports_lf_delimiters() {
        let mut buffer = String::from("data: {\"ok\":true}\n\nrest");

        let (end, delim) = next_sse_block_range(&buffer).expect("block");
        assert_eq!(&buffer[..end], "data: {\"ok\":true}");
        assert_eq!(delim, 2);
        buffer.drain(..end + delim);
        assert_eq!(buffer, "rest");
    }

    #[test]
    fn next_sse_block_range_returns_the_earliest_delimiter() {
        let mut buffer = String::from("data: first\n\n\r\ndata: second\r\n\r\nrest");

        let (end, delimiter) = next_sse_block_range(&buffer).expect("earliest block");
        assert_eq!(&buffer[..end], "data: first");
        assert_eq!(delimiter, 2);
        buffer.drain(..end + delimiter);
        assert_eq!(buffer, "\r\ndata: second\r\n\r\nrest");
    }

    #[test]
    fn next_sse_block_range_supports_crlf_delimiters() {
        let mut buffer = String::from("data: {\"ok\":true}\r\n\r\nrest");

        let (end, delim) = next_sse_block_range(&buffer).expect("block");
        assert_eq!(&buffer[..end], "data: {\"ok\":true}");
        assert_eq!(delim, 4);
        buffer.drain(..end + delim);
        assert_eq!(buffer, "rest");
    }

    #[test]
    fn next_sse_block_range_handles_mixed_line_endings_across_chunks() {
        for delimiter in ["\n\n", "\r\n\r\n", "\n\r\n", "\r\n\n"] {
            let first = "event: message\r\ndata: first";
            let input = format!("{first}{delimiter}data: second\n\npartial");

            // Exercise every possible transport split, including inside CRLF.
            for split in 0..=input.len() {
                let mut buffer = String::new();
                let mut blocks = Vec::new();
                for chunk in [&input[..split], &input[split..]] {
                    buffer.push_str(chunk);
                    while let Some((end, delim)) = next_sse_block_range(&buffer) {
                        blocks.push(buffer[..end].to_string());
                        buffer.drain(..end + delim);
                    }
                }
                assert_eq!(
                    blocks,
                    [first, "data: second"],
                    "{delimiter:?}, split {split}"
                );
                assert_eq!(buffer, "partial");
            }

            // A complete mixed delimiter must dispatch without another event.
            let mut buffer = format!("{first}{delimiter}");
            let (end, _delim) = next_sse_block_range(&buffer).expect("block");
            assert_eq!(&buffer[..end], first);
            buffer.drain(..end + _delim);
            assert!(buffer.is_empty());
        }
    }

    // ------------------------------------------------------------------
    // Utf8TailDecoder tests
    // ------------------------------------------------------------------

    /// Feed the whole input in one chunk.
    fn decode_all(input: &str) -> String {
        let mut decoder = Utf8TailDecoder::default();
        let mut buffer = String::new();
        decoder.append(&mut buffer, input.as_bytes());
        buffer
    }

    /// Feed the input one byte at a time, the worst case for the cross-chunk
    /// character logic.
    fn decode_byte_by_byte(input: &str) -> String {
        let mut decoder = Utf8TailDecoder::default();
        let mut buffer = String::new();
        for byte in input.as_bytes() {
            decoder.append(&mut buffer, std::slice::from_ref(byte));
        }
        buffer
    }

    #[test]
    fn ascii_passthrough() {
        assert_eq!(decode_all("hello world"), "hello world");
        assert_eq!(decode_byte_by_byte("hello world"), "hello world");
    }

    #[test]
    fn complete_multibyte_in_single_chunk() {
        assert_eq!(decode_all("ni hao shi jie"), "ni hao shi jie");
        assert_eq!(decode_byte_by_byte("ni hao shi jie"), "ni hao shi jie");
    }

    #[test]
    fn split_multibyte_across_two_chunks() {
        // U+4F60 is three bytes: E4 BD A0
        let bytes = "\u{4F60}".as_bytes();
        assert_eq!(bytes.len(), 3);

        let mut decoder = Utf8TailDecoder::default();
        let mut buffer = String::new();

        // Chunk 1: first 2 bytes (incomplete) -> nothing decodes yet
        decoder.append(&mut buffer, &bytes[..2]);
        assert_eq!(buffer, "");

        // Chunk 2: last byte completes the character
        decoder.append(&mut buffer, &bytes[2..]);
        assert_eq!(buffer, "\u{4F60}");

        // A third chunk is unaffected, proving the tail was cleared
        decoder.append(&mut buffer, b"x");
        assert_eq!(buffer, "\u{4F60}x");
    }

    #[test]
    fn split_four_byte_char_across_chunks() {
        // U+1F600 is four bytes: F0 9F 98 80
        assert_eq!(decode_byte_by_byte("\u{1F600}"), "\u{1F600}");
        assert_eq!(decode_byte_by_byte("a\u{1F600}b"), "a\u{1F600}b");
    }

    #[test]
    fn mixed_ascii_and_split_multibyte() {
        assert_eq!(decode_byte_by_byte("hi\u{4F60}"), "hi\u{4F60}");
    }

    #[test]
    fn multiple_split_characters_in_sequence() {
        assert_eq!(decode_byte_by_byte("\u{4F60}\u{597D}"), "\u{4F60}\u{597D}");

        // Every split point, including inside a character, must reassemble.
        let input = "\u{6570}\u{636E}\u{6D41}\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}";
        let bytes = input.as_bytes();
        for split in 0..=bytes.len() {
            let mut decoder = Utf8TailDecoder::default();
            let mut buffer = String::new();
            decoder.append(&mut buffer, &bytes[..split]);
            decoder.append(&mut buffer, &bytes[split..]);
            assert_eq!(buffer, input, "split at byte {split}");
        }
    }

    #[test]
    fn empty_chunks_are_harmless() {
        let mut decoder = Utf8TailDecoder::default();
        let mut buffer = String::new();

        decoder.append(&mut buffer, b"");
        assert_eq!(buffer, "");

        decoder.append(&mut buffer, b"ok");
        assert_eq!(buffer, "ok");

        decoder.append(&mut buffer, b"");
        assert_eq!(buffer, "ok");
    }

    #[test]
    fn sse_json_with_chinese_split_at_boundary() {
        // Simulates an SSE data line with Chinese content split across chunks
        let json_line = "data: {\"text\":\"\u{4F60}\u{597D}\"}\n\n";
        let bytes = json_line.as_bytes();

        // Find where the first multi-byte character starts and split it
        let split_point = bytes
            .windows(3)
            .position(|w| w == "\u{4F60}".as_bytes())
            .unwrap()
            + 1;

        let mut decoder = Utf8TailDecoder::default();
        let mut buffer = String::new();
        decoder.append(&mut buffer, &bytes[..split_point]);
        decoder.append(&mut buffer, &bytes[split_point..]);

        assert_eq!(buffer, json_line);

        // Verify the buffer can be parsed as SSE with valid JSON
        let data = strip_sse_field(buffer.lines().next().unwrap(), "data").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed["text"], "\u{4F60}\u{597D}");
    }

    #[test]
    fn invalid_bytes_flushed_immediately_not_accumulated() {
        // 0xFF is never valid in UTF-8, so each byte becomes one U+FFFD
        // instead of being stashed as a pending character.
        assert_eq!(decode_all("hi\u{FFFD}ok"), "hi\u{FFFD}ok");
        assert_eq!(decode_all("hi\u{FFFD}\u{FFFD}ok"), "hi\u{FFFD}\u{FFFD}ok");
    }

    #[test]
    fn invalid_byte_in_slow_path_flushed_immediately() {
        let mut decoder = Utf8TailDecoder::default();
        let mut buf = String::new();

        // Prime the tail with an incomplete sequence (first byte of U+4F60)
        decoder.append(&mut buf, &"\u{4F60}".as_bytes()[..1]);
        assert_eq!(buf, "");

        // Next chunk starts with an invalid byte: the stale tail and the
        // invalid byte are both flushed, not accumulated.
        decoder.append(&mut buf, b"\xFFworld");
        assert_eq!(buf, "\u{FFFD}\u{FFFD}world");

        // Valid data after an invalid byte in the slow path still lands
        decoder.append(&mut buf, b"tail");
        assert_eq!(buf, "\u{FFFD}\u{FFFD}worldtail");
    }

    #[test]
    fn pending_tail_survives_an_empty_chunk() {
        let mut decoder = Utf8TailDecoder::default();
        let mut buf = String::new();

        decoder.append(&mut buf, &"\u{4F60}".as_bytes()[..2]);
        decoder.append(&mut buf, b"");
        decoder.append(&mut buf, &"\u{4F60}".as_bytes()[2..]);
        assert_eq!(buf, "\u{4F60}");
    }

    #[test]
    fn orphan_continuation_bytes_each_produce_one_replacement() {
        // 0x80 can never start a sequence, so each byte is invalid on its own.
        let orphans = "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}";
        assert_eq!(decode_all(orphans), orphans);
        assert_eq!(decode_byte_by_byte(orphans), orphans);
    }

    #[test]
    fn decoder_is_reusable_across_streams() {
        let text = "mixed \u{6DF7}\u{5408} content with \u{1F600} emoji\n\ndata: {\"ok\":true}\n\n";
        assert_eq!(decode_all(text), text);
        assert_eq!(decode_byte_by_byte(text), text);
    }

    #[test]
    fn pending_tail_meeting_invalid_bytes_repeatedly_terminates() {
        // 每个 chunk 都只送一个悬挂首字节、下一 chunk 紧跟一个无效字节。这种形状
        // 曾让 decode_from_tail 的错误路径把同一个前导字节反复重拼、无限递归直到
        // 栈溢出，所以这里显式断言它收敛，而不是只测一次就以为没事。
        let mut decoder = Utf8TailDecoder::default();
        let mut buffer = String::new();
        for _ in 0..64 {
            decoder.append(&mut buffer, &"\u{4F60}".as_bytes()[..1]);
            decoder.append(&mut buffer, b"\xFF");
        }
        decoder.append(&mut buffer, b"done");

        let replacements = buffer.chars().filter(|c| *c == '\u{FFFD}').count();
        assert!(
            replacements >= 64,
            "每一轮悬挂+无效至少产出一个替换字符，实际 {replacements}"
        );
        assert_eq!(buffer.trim_start_matches('\u{FFFD}'), "done");
    }

    #[test]
    fn frame_sse_event_frames_a_payload_between_the_separators() {
        let payload = serde_json::json!({"type": "content_block_delta", "index": 3});
        let framed = frame_sse_event("content_block_delta", &payload);

        assert_eq!(
            framed,
            Bytes::from(
                // preserve_order 下按声明序输出，不是字母序
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":3}\n\n"
            )
        );
    }

    #[test]
    fn frame_sse_event_round_trips_through_the_block_splitter() {
        // 成帧出来的字节必须能被本模块的扫描器切成完整的一块
        let payload =
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}});
        let framed = frame_sse_event("message_delta", &payload);
        let text = std::str::from_utf8(&framed).expect("valid utf-8");

        let (end, delim) = next_sse_block_range(text).expect("one block");
        assert_eq!(
            &text[..end],
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}"
        );
        assert_eq!(end + delim, text.len());
        assert_eq!(
            strip_sse_field(text.lines().next().unwrap(), "event"),
            Some("message_delta")
        );
    }

    #[test]
    fn frame_sse_event_keeps_multibyte_payloads_intact() {
        let payload = serde_json::json!({"type": "content_block_delta", "delta": {"text": "\u{4F60}\u{597D}"}});
        let framed = frame_sse_event("content_block_delta", &payload);
        let text = std::str::from_utf8(&framed).expect("valid utf-8");

        assert!(text.contains("\u{4F60}\u{597D}"), "{text}");
        assert!(text.ends_with("\n\n"), "{text}");
    }

    #[test]
    fn scan_sse_block_borrows_a_single_data_line() {
        let (event, payload) =
            scan_sse_block("event: response.output_text.delta\ndata: {\"delta\":\"hi\"}");

        assert_eq!(event, Some("response.output_text.delta"));
        let payload = payload.expect("data line");
        assert!(matches!(payload, Cow::Borrowed(_)), "单行 data 不该分配");
        assert_eq!(&*payload, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn sse_block_data_borrows_a_single_data_line() {
        let data = sse_block_data("event: message\ndata: {\"a\":1}").expect("data line");
        assert!(matches!(data, Cow::Borrowed(_)), "单行 data 不该分配");
        assert_eq!(&*data, "{\"a\":1}");
    }

    #[test]
    fn sse_block_data_joins_multiple_data_lines_with_newlines() {
        assert_eq!(
            &*sse_block_data("data: {\"a\":1,\ndata: \"b\":2}").expect("data lines"),
            "{\"a\":1,\n\"b\":2}"
        );
    }

    #[test]
    fn sse_block_data_reports_no_payload_without_a_data_line() {
        assert!(sse_block_data("event: ping\n").is_none());
        assert!(sse_block_data(": comment only").is_none());
    }

    #[test]
    fn sse_block_payload_borrows_a_single_data_line() {
        assert_eq!(
            sse_block_payload("event: message\ndata: {\"a\":1}"),
            Some("{\"a\":1}")
        );
        assert_eq!(
            sse_block_payload("data:{\"a\":1}"),
            Some("{\"a\":1}"),
            "data 后面没空格也认"
        );
        assert_eq!(sse_block_payload("event: ping"), None, "没有 data 行");
        assert_eq!(sse_block_payload(": comment only"), None);
        assert_eq!(
            sse_block_payload("data: {\"a\":1,\ndata: \"b\":2}"),
            None,
            "多行 data 交给调用方的慢路径去拼"
        );
    }

    #[test]
    fn sse_data_type_reads_only_the_top_level_type() {
        assert_eq!(
            sse_data_type("{\"type\":\"response.completed\"}"),
            Some("response.completed")
        );
        assert_eq!(
            sse_data_type("{\"type\":\"x\",\"nested\":{\"type\":\"y\"}}"),
            Some("x"),
            "只取顶层 type"
        );
        assert_eq!(
            sse_data_type(
                "{\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1}}}"
            ),
            Some("response.completed"),
            "长载荷也只读 type"
        );
        assert_eq!(sse_data_type("[DONE]"), None);
        assert_eq!(sse_data_type("{\"type\":7}"), None, "type 不是字符串");
        assert_eq!(sse_data_type("not json"), None);
    }

    #[test]
    fn scan_sse_block_joins_multiple_data_lines_with_newlines() {
        let (event, payload) = scan_sse_block("event: chunk\ndata: {\"a\":1,\ndata: \"b\":2}");

        assert_eq!(event, Some("chunk"));
        let payload = payload.expect("data lines");
        assert!(matches!(payload, Cow::Owned(_)), "多行必须拼一份");
        assert_eq!(&*payload, "{\"a\":1,\n\"b\":2}");
    }

    #[test]
    fn scan_sse_block_takes_the_last_event_line() {
        // 与旧内联循环一致：同一块里重复的 event: 后者胜出
        let (event, payload) = scan_sse_block("event: first\nevent: second\ndata: {}");
        assert_eq!(event, Some("second"));
        assert_eq!(&*payload.expect("data"), "{}");
    }

    #[test]
    fn scan_sse_block_reports_no_payload_without_a_data_line() {
        let (event, payload) = scan_sse_block("event: ping\n");
        assert_eq!(event, Some("ping"));
        assert!(payload.is_none());

        let (event, payload) = scan_sse_block(": comment only");
        assert!(event.is_none());
        assert!(payload.is_none());
    }

    #[test]
    fn sse_blocks_splits_mixed_line_endings() {
        for delimiter in ["\n\n", "\r\n\r\n", "\n\r\n", "\r\n\n"] {
            let first = "event: message\r\ndata: first";
            let input = format!("{first}{delimiter}data: second\n\npartial");
            let mut blocks = SseBlocks::new(&input);
            assert_eq!(
                blocks.by_ref().collect::<Vec<_>>(),
                [first, "data: second"],
                "{delimiter:?}"
            );
            assert_eq!(blocks.remainder(), "partial");
        }
    }

    #[test]
    fn sse_blocks_frames_a_whole_body() {
        // A long event, CRLF and mixed delimiters, an empty event and multi-byte
        // text: the whole-body iterator must frame exactly like the streaming path.
        let long = format!("data: {}", "\u{4e2d}\u{6587}\n".repeat(200));
        // 内容行自带的换行符被并进紧随的空行分隔符：块文本到此为止，下一块从
        // 剩下的 \r\n 开始。逐字节喂入的流式扫描器也是这个切法。
        let long_block = long.trim_end_matches('\n');
        let input =
            format!("{long}\r\n\r\nevent: a\r\ndata: 1\n\r\n\n\ndata: \u{1F600}\r\n\ndata: tail");
        let mut whole = SseBlocks::new(&input);
        assert_eq!(
            whole.by_ref().map(str::to_string).collect::<Vec<_>>(),
            [long_block, "\r\nevent: a\r\ndata: 1", "", "data: \u{1F600}"]
        );
        assert_eq!(whole.remainder(), "data: tail");
    }
}
