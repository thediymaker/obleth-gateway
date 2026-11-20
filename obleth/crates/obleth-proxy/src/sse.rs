//! Server-sent events, read as they arrive.
//!
//! Every reader of an upstream event stream splits it here: the failure watch,
//! the output monitor, the usage rewriter, the Responses and Messages
//! translators, the tool-stream loops and speculation. So they all agree on
//! where an event ends and what its data is.
//!
//! An event ends at a blank line. A line ends in `\n` or `\r\n`; a bare `\r`
//! line ending, which no upstream sends, is not recognised. An event's data is
//! its `data:` lines, each without the one space after the colon, joined by
//! `\n`, as the SSE specification reads them.

use std::borrow::Cow;
use std::ops::Range;

use bytes::Bytes;
use serde_json::Value;

use crate::energy::EnergyFigures;

/// Where `needle` first occurs in `haystack`.
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(haystack, needle)
}

pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}

/// Whether bytes end at an event boundary: a line ending, then a blank line.
pub(crate) fn ends_at_boundary(bytes: &[u8]) -> bool {
    bytes.ends_with(b"\n\n") || bytes.ends_with(b"\n\r\n")
}

/// The end of the first event that ends at or after `from`: `Ok` just past
/// its blank line, or `Err` with where to look again once more bytes arrive.
/// Only a line ending can start a boundary, so `from` may sit anywhere.
fn event_end(buf: &[u8], from: usize) -> Result<usize, usize> {
    let mut at = from;
    while let Some(newline) = memchr::memchr(b'\n', &buf[at..]).map(|i| at + i) {
        match buf.get(newline + 1) {
            Some(b'\n') => return Ok(newline + 2),
            Some(b'\r') => match buf.get(newline + 2) {
                Some(b'\n') => return Ok(newline + 3),
                Some(_) => {}
                None => return Err(newline),
            },
            Some(_) => {}
            None => return Err(newline),
        }
        at = newline + 1;
    }
    Err(buf.len())
}

/// What a [`Splitter`] hands out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piece<'a> {
    /// One whole event, up to and including its blank line.
    Event(&'a [u8]),
    /// Part of an event longer than the cap, handed out as it arrives so it
    /// is never held whole; `last` on the part that ends it.
    Oversized { bytes: &'a [u8], last: bool },
}

/// Bytes in, events out. One buffer is reused for the life of the stream,
/// and each byte is scanned for a boundary once.
pub(crate) struct Splitter {
    buf: Vec<u8>,
    /// The first byte not handed out yet.
    start: usize,
    /// Where the boundary search resumes.
    scan: usize,
    /// The longest event held whole.
    cap: usize,
    /// The event being read outgrew the cap.
    oversized: bool,
}

impl Splitter {
    /// A splitter that holds an event up to `cap` bytes, and hands a longer
    /// one out in [`Piece::Oversized`] parts.
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            scan: 0,
            cap,
            oversized: false,
        }
    }

    /// A splitter that holds any event whole.
    pub(crate) fn unbounded() -> Self {
        Self::new(usize::MAX)
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.scan -= self.start;
            self.start = 0;
        }
        self.buf.extend_from_slice(chunk);
    }

    /// Between events: nothing held, and no oversized event under way.
    pub(crate) fn is_idle(&self) -> bool {
        self.start == self.buf.len() && !self.oversized
    }

    /// The next whole event, or the next part of an oversized one; `None`
    /// until more bytes arrive.
    pub(crate) fn next(&mut self) -> Option<Piece<'_>> {
        let (range, oversized) = self.next_range()?;
        Some(self.piece(range, oversized))
    }

    /// The next event of an [`unbounded`](Self::unbounded) splitter, which
    /// hands out no oversized parts.
    pub(crate) fn next_event(&mut self) -> Option<&[u8]> {
        let (range, _) = self.next_range()?;
        Some(&self.buf[range])
    }

    /// Whatever is held once the stream has ended, as its last event: an
    /// upstream may close without a final blank line.
    pub(crate) fn finish(&mut self) -> Option<Piece<'_>> {
        if self.start == self.buf.len() {
            self.oversized = false;
            return None;
        }
        let range = self.start..self.buf.len();
        self.start = self.buf.len();
        self.scan = self.start;
        let oversized = std::mem::take(&mut self.oversized) || range.len() > self.cap;
        Some(self.piece(range, oversized.then_some(true)))
    }

    fn piece(&self, range: Range<usize>, oversized: Option<bool>) -> Piece<'_> {
        let bytes = &self.buf[range];
        match oversized {
            None => Piece::Event(bytes),
            Some(last) => Piece::Oversized { bytes, last },
        }
    }

    /// The next piece's bytes, and `Some(last)` when it is part of an
    /// oversized event.
    fn next_range(&mut self) -> Option<(Range<usize>, Option<bool>)> {
        let start = self.start;
        if start == self.buf.len() {
            return None;
        }
        match event_end(&self.buf, self.scan.max(start)) {
            Ok(end) => {
                self.start = end;
                self.scan = end;
                let oversized = std::mem::take(&mut self.oversized) || end - start > self.cap;
                Some((start..end, oversized.then_some(true)))
            }
            Err(resume) => {
                self.scan = resume;
                if !self.oversized && self.buf.len() - start <= self.cap {
                    return None;
                }
                // Hand out all but the bytes a boundary may still start in.
                self.oversized = true;
                if resume == start {
                    return None;
                }
                self.start = resume;
                Some((start..resume, Some(false)))
            }
        }
    }
}

/// An event's lines, without their line endings.
pub(crate) fn lines(event: &[u8]) -> impl Iterator<Item = &[u8]> {
    event
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
}

/// An event's data: its `data:` lines joined by `\n`, each without the one
/// space after the colon. Empty when it has none.
pub(crate) fn data_payload(event: &[u8]) -> Cow<'_, [u8]> {
    let mut first: Option<&[u8]> = None;
    let mut joined: Option<Vec<u8>> = None;
    for line in lines(event) {
        let Some(data) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if let Some(joined) = joined.as_mut() {
            joined.push(b'\n');
            joined.extend_from_slice(data);
        } else if let Some(first) = first {
            let mut both = Vec::with_capacity(first.len() + 1 + data.len());
            both.extend_from_slice(first);
            both.push(b'\n');
            both.extend_from_slice(data);
            joined = Some(both);
        } else {
            first = Some(data);
        }
    }
    match joined {
        Some(joined) => Cow::Owned(joined),
        None => Cow::Borrowed(first.unwrap_or_default()),
    }
}

/// An event's data as text with surrounding whitespace trimmed, or `None`
/// when it carries none (a comment or heartbeat).
pub(crate) fn data_text(event: &[u8]) -> Option<Cow<'_, str>> {
    let text = match data_payload(event) {
        Cow::Borrowed(bytes) => match String::from_utf8_lossy(bytes) {
            Cow::Borrowed(text) => Cow::Borrowed(text.trim()),
            Cow::Owned(text) => Cow::Owned(text.trim().to_string()),
        },
        Cow::Owned(bytes) => Cow::Owned(String::from_utf8_lossy(&bytes).trim().to_string()),
    };
    (!text.is_empty()).then_some(text)
}

/// An event's name (its `event:` field), trimmed.
pub(crate) fn event_name(event: &[u8]) -> Option<&[u8]> {
    lines(event)
        .filter_map(|line| line.strip_prefix(b"event:"))
        .last()
        .map(<[u8]>::trim_ascii)
}

/// Above this, an event is generated content rather than usage, and the
/// usage rewriter passes it on as it arrives instead of holding it.
pub(crate) const USAGE_MAX_EVENT: usize = 64 * 1024;

/// The largest JSON answer held to add energy to its usage.
const USAGE_MAX_BODY: usize = 1024 * 1024;

/// What the usage rewriter passes on for a chunk: the bytes for the client,
/// and the same bytes without the energy added, for the response cache (a
/// cache hit uses no energy) and the output monitor.
pub(crate) struct Rewritten {
    pub(crate) client: Bytes,
    pub(crate) unstamped: Bytes,
}

impl Rewritten {
    fn same(bytes: Bytes) -> Self {
        Self {
            client: bytes.clone(),
            unstamped: bytes,
        }
    }
}

enum Body {
    Sse(Splitter),
    /// A whole JSON answer, held until it ends, as a client can read it only
    /// whole; `passing` once it outgrew [`USAGE_MAX_BODY`].
    Json {
        held: Vec<u8>,
        passing: bool,
    },
}

/// Rewrites the usage of an answer on its way to the client, in one pass:
/// drops the usage-only event the gateway asked the upstream for on a
/// client's behalf (`strip`), and adds the request's measured energy and
/// carbon to the usage a client receives (`stamp`), as `usage.energy_wh` and
/// `usage.co2_g`.
///
/// In a stream only events that mention `prompt_tokens` are parsed, and every
/// other byte passes through as is; a chunk of whole events without usage
/// passes without a copy, so a live stream gains no latency. The figures are
/// read as the usage goes by, the end of the answer, so they match what
/// settlement records to within that moment.
pub(crate) struct UsageRewriter {
    strip: bool,
    stamp: bool,
    body: Body,
    /// An oversized event was passed on; logged once per stream.
    overflowed: bool,
}

enum EventRewrite {
    Keep,
    Drop,
    Stamped(Vec<u8>),
}

impl UsageRewriter {
    /// A rewriter for an answer of this content type, or `None` when there is
    /// nothing to do: neither asked for, or a body it can't read.
    pub(crate) fn new(content_type: &str, strip: bool, stamp: bool) -> Option<Self> {
        let sse = content_type.starts_with("text/event-stream");
        let body = if !sse && content_type.contains("json") {
            // Only a stream carries a usage-only event to strip.
            if !stamp {
                return None;
            }
            Body::Json {
                held: Vec::new(),
                passing: false,
            }
        } else {
            // Stripping reads any body as events; only a stream is stamped.
            let rewrites = strip || (stamp && sse);
            if !rewrites {
                return None;
            }
            Body::Sse(Splitter::new(USAGE_MAX_EVENT))
        };
        Some(Self {
            strip,
            stamp: stamp && (sse || matches!(body, Body::Json { .. })),
            body,
            overflowed: false,
        })
    }

    pub(crate) fn push(&mut self, chunk: Bytes, figures: impl Fn() -> EnergyFigures) -> Rewritten {
        let events = match &mut self.body {
            Body::Json { held, passing } => {
                if *passing {
                    return Rewritten::same(chunk);
                }
                held.extend_from_slice(&chunk);
                if held.len() > USAGE_MAX_BODY {
                    *passing = true;
                    return Rewritten::same(Bytes::from(std::mem::take(held)));
                }
                return Rewritten::same(Bytes::new());
            }
            Body::Sse(events) => events,
        };
        // Common case: a chunk of whole events with no usage in it.
        if events.is_idle() && ends_at_boundary(&chunk) && !contains(&chunk, b"prompt_tokens") {
            return Rewritten::same(chunk);
        }
        events.push(&chunk);
        let mut out = Vec::with_capacity(chunk.len() + 64);
        // Set once an event is stamped: the same bytes, unstamped.
        let mut plain: Option<Vec<u8>> = None;
        let emit = |out: &mut Vec<u8>, plain: &mut Option<Vec<u8>>, bytes: &[u8]| {
            out.extend_from_slice(bytes);
            if let Some(plain) = plain.as_mut() {
                plain.extend_from_slice(bytes);
            }
        };
        while let Some(piece) = events.next() {
            match piece {
                Piece::Oversized { bytes, .. } => {
                    if !self.overflowed {
                        self.overflowed = true;
                        tracing::debug!(
                            held_bytes = bytes.len(),
                            "usage rewriter met an event past its cap; passing it on unread"
                        );
                    }
                    emit(&mut out, &mut plain, bytes);
                }
                Piece::Event(event) => {
                    match rewrite_event(event, self.strip, self.stamp, &figures) {
                        EventRewrite::Keep => emit(&mut out, &mut plain, event),
                        EventRewrite::Drop => {}
                        EventRewrite::Stamped(stamped) => {
                            plain
                                .get_or_insert_with(|| out.clone())
                                .extend_from_slice(event);
                            out.extend_from_slice(&stamped);
                        }
                    }
                }
            }
        }
        let client = Bytes::from(out);
        match plain {
            Some(plain) => Rewritten {
                client,
                unstamped: Bytes::from(plain),
            },
            None => Rewritten::same(client),
        }
    }

    /// What is still held once the answer has ended: a JSON answer, stamped,
    /// or a stream's unterminated last event, as received.
    pub(crate) fn finish(self, figures: impl Fn() -> EnergyFigures) -> Rewritten {
        match self.body {
            Body::Json { held, passing } => {
                let held = Bytes::from(held);
                if passing || held.is_empty() || !self.stamp {
                    return Rewritten::same(held);
                }
                Rewritten {
                    client: stamp_json(held.clone(), figures()),
                    unstamped: held,
                }
            }
            Body::Sse(mut events) => match events.finish() {
                Some(Piece::Event(bytes) | Piece::Oversized { bytes, .. }) => {
                    Rewritten::same(Bytes::copy_from_slice(bytes))
                }
                None => Rewritten::same(Bytes::new()),
            },
        }
    }
}

fn rewrite_event(
    event: &[u8],
    strip: bool,
    stamp: bool,
    figures: &dyn Fn() -> EnergyFigures,
) -> EventRewrite {
    if !contains(event, b"prompt_tokens") {
        return EventRewrite::Keep;
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(&data_payload(event)) else {
        return EventRewrite::Keep;
    };
    if strip && is_usage_only(&value) {
        return EventRewrite::Drop;
    }
    if stamp && stamp_usage(&mut value, figures()) {
        return EventRewrite::Stamped(stamped_event(event, &value));
    }
    EventRewrite::Keep
}

/// A chunk carrying only usage: a `usage` object and no choices. Content
/// chunks that also carry usage (continuous usage stats) are not dropped.
fn is_usage_only(value: &Value) -> bool {
    let has_usage = value.get("usage").is_some_and(Value::is_object);
    let no_choices = match value.get("choices") {
        None => true,
        Some(c) => c.as_array().is_some_and(|a| a.is_empty()),
    };
    has_usage && no_choices
}

/// `event` with its data replaced by `value`, keeping its other fields and
/// its line endings.
fn stamped_event(event: &[u8], value: &Value) -> Vec<u8> {
    let end: &[u8] = if event.ends_with(b"\r\n\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut out = Vec::with_capacity(event.len() + 64);
    for line in lines(event) {
        if line.starts_with(b"data:") || line.trim_ascii().is_empty() {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(end);
    }
    out.extend_from_slice(b"data: ");
    // Writing a Value to a Vec cannot fail.
    let _ = serde_json::to_writer(&mut out, value);
    out.extend_from_slice(end);
    out.extend_from_slice(end);
    out
}

/// Writes `energy_wh` and `co2_g` into a body's `usage` object (chat and text
/// completion shape), rounded to a millionth. False when there is no such
/// object or nothing was measured.
pub(crate) fn stamp_usage(value: &mut Value, figures: EnergyFigures) -> bool {
    let Some(usage) = value.get_mut("usage").and_then(Value::as_object_mut) else {
        return false;
    };
    if !usage.contains_key("prompt_tokens")
        || !figures.energy_wh.is_finite()
        || figures.energy_wh <= 0.0
    {
        return false;
    }
    let round = |n: f64| (n * 1e6).round() / 1e6;
    usage.insert(
        "energy_wh".into(),
        serde_json::json!(round(figures.energy_wh)),
    );
    if figures.co2_g.is_finite() && figures.co2_g > 0.0 {
        usage.insert("co2_g".into(), serde_json::json!(round(figures.co2_g)));
    }
    true
}

fn stamp_json(body: Bytes, figures: EnergyFigures) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    if !stamp_usage(&mut value, figures) {
        return body;
    }
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

/// The energy stamp for a whole buffered body: JSON, or SSE the gateway
/// assembled for a client that asked to stream.
pub(crate) fn stamp_body(body: Bytes, content_type: &str, figures: EnergyFigures) -> Bytes {
    if !content_type.starts_with("text/event-stream") {
        return stamp_json(body, figures);
    }
    let Some(mut rewriter) = UsageRewriter::new(content_type, false, true) else {
        return body;
    };
    let mut out = rewriter.push(body, || figures).client.to_vec();
    out.extend_from_slice(&rewriter.finish(|| figures).client);
    Bytes::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every event and oversized part a splitter hands out for `chunks`,
    /// with what `finish` hands out last.
    fn split(cap: usize, chunks: &[&[u8]]) -> Vec<(Vec<u8>, Option<bool>)> {
        let mut splitter = Splitter::new(cap);
        let mut out = Vec::new();
        let mut take = |piece: Piece<'_>| match piece {
            Piece::Event(e) => out.push((e.to_vec(), None)),
            Piece::Oversized { bytes, last } => out.push((bytes.to_vec(), Some(last))),
        };
        for chunk in chunks {
            splitter.push(chunk);
            while let Some(piece) = splitter.next() {
                take(piece);
            }
        }
        if let Some(piece) = splitter.finish() {
            take(piece);
        }
        out
    }

    fn events(chunks: &[&[u8]]) -> Vec<String> {
        split(usize::MAX, chunks)
            .into_iter()
            .map(|(e, kind)| {
                assert_eq!(kind, None);
                String::from_utf8(e).unwrap()
            })
            .collect()
    }

    /// Recorded shapes of the upstreams in use: vLLM and SGLang (LF, a usage
    /// chunk, `[DONE]`), an OpenAI-style proxy with keep-alive comments, and
    /// a CRLF-framed one with named events and multi-line data.
    fn recorded() -> Vec<String> {
        let chunk = |content: &str| {
            format!(
                "data: {{\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{content}\"}},\"logprobs\":null,\"finish_reason\":null}}]}}\n\n"
            )
        };
        let usage = "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"total_tokens\":15,\"completion_tokens\":3}}\n\n";
        let finish = "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let vllm = format!(
            "{}{}{}{finish}{usage}data: [DONE]\n\n",
            chunk(""),
            chunk("Hel"),
            chunk("lo 猫 🐈")
        );
        let openai = format!(
            ": keep-alive\n\n{}: keep-alive\n\n{}{finish}{usage}data: [DONE]\n\n",
            chunk("a"),
            chunk("b")
        );
        let crlf = vllm.replace("\n\n", "\r\n\r\n");
        let named = "event: message\r\nid: 1\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\nevent: error\r\ndata: {\"error\":{\"message\":\"x\"}}\r\n\r\n".to_string();
        vec![vllm, openai, crlf, named]
    }

    #[test]
    fn events_end_at_a_blank_line_with_either_line_ending() {
        assert_eq!(
            events(&[b"data: a\n\ndata: b\r\n\r\n: ping\n\ndata: c\n\r\ndata: d"]),
            [
                "data: a\n\n",
                "data: b\r\n\r\n",
                ": ping\n\n",
                "data: c\n\r\n",
                "data: d"
            ]
        );
        // An event of two lines, and a blank line on its own.
        assert_eq!(
            events(&[b"event: x\ndata: 1\n\n\n\n"]),
            ["event: x\ndata: 1\n\n", "\n\n"]
        );
    }

    #[test]
    fn every_split_of_a_recorded_stream_gives_the_same_events() {
        for wire in recorded() {
            let whole = events(&[wire.as_bytes()]);
            assert_eq!(whole.concat(), wire);
            let bytes = wire.as_bytes();
            for cut in 0..=bytes.len() {
                assert_eq!(
                    events(&[&bytes[..cut], &bytes[cut..]]),
                    whole,
                    "cut at {cut}"
                );
            }
            // One byte at a time.
            let bytes: Vec<&[u8]> = bytes.chunks(1).collect();
            assert_eq!(events(&bytes), whole);
        }
    }

    #[test]
    fn a_character_split_across_chunks_is_read_whole() {
        let frame = "data: {\"text\":\"猫\"}\n\n".as_bytes();
        let (a, b) = frame.split_at(frame.iter().position(|c| *c >= 0x80).unwrap() + 1);
        let mut splitter = Splitter::unbounded();
        splitter.push(a);
        assert!(splitter.next_event().is_none(), "no whole event yet");
        splitter.push(b);
        let event = splitter.next_event().unwrap();
        assert_eq!(data_text(event).as_deref(), Some("{\"text\":\"猫\"}"));
        assert!(splitter.is_idle());
    }

    #[test]
    fn an_event_past_the_cap_is_handed_out_in_parts_and_the_next_is_whole() {
        let long = format!("data: {}\n\n", "x".repeat(100));
        let wire = format!("{long}data: after\n\n");
        let bytes = wire.as_bytes();
        for piece in [1, 7, 40, bytes.len()] {
            let chunks: Vec<&[u8]> = bytes.chunks(piece).collect();
            let out = split(32, &chunks);
            let (last, rest) = out.split_last().unwrap();
            assert_eq!(
                last,
                &(b"data: after\n\n".to_vec(), None),
                "pieces of {piece}"
            );
            // The long event comes out in parts, the last one marked, and
            // nothing held is ever more than the cap plus a chunk.
            assert_eq!(rest.last().unwrap().1, Some(true));
            assert!(rest[..rest.len() - 1]
                .iter()
                .all(|(_, kind)| *kind == Some(false)));
            let joined: Vec<u8> = rest.iter().flat_map(|(b, _)| b.clone()).collect();
            assert_eq!(joined, long.as_bytes());
        }
        // A blank line split across parts still ends the event.
        let out = split(10, &[b"data: 1234567\n", b"\r", b"\ndata: z\n\n"]);
        assert_eq!(out.last().unwrap(), &(b"data: z\n\n".to_vec(), None));
    }

    #[test]
    fn data_is_read_as_the_specification_reads_it() {
        assert_eq!(&data_payload(b"data: {\"a\":1}\n\n")[..], b"{\"a\":1}");
        assert_eq!(&data_payload(b"data:x\r\n\r\n")[..], b"x");
        assert_eq!(&data_payload(b"data:  two spaces\n\n")[..], b" two spaces");
        assert_eq!(
            &data_payload(b"event: e\ndata: a\nid: 1\ndata: b\n\n")[..],
            b"a\nb"
        );
        assert_eq!(&data_payload(b": comment\n\n")[..], b"");
        assert_eq!(data_text(b"data: [DONE]\n\n").as_deref(), Some("[DONE]"));
        assert_eq!(data_text(b": ping\n\n"), None);
        assert_eq!(data_text(b"data: \n\n"), None);
        assert_eq!(
            event_name(b"event: error \r\ndata: x\r\n\r\n"),
            Some(&b"error"[..])
        );
        assert_eq!(event_name(b"data: x\n\n"), None);
    }

    #[test]
    fn the_byte_search_finds_the_first_match() {
        assert_eq!(find_bytes(b"abcabc", b"ca"), Some(2));
        assert_eq!(find_bytes(b"abc", b"abcd"), None);
        assert!(contains(b"\"prompt_tokens\":1", b"prompt_tokens"));
        assert!(!contains(b"", b"x"));
    }

    fn rewrite(strip: bool, stamp: bool, chunks: &[&[u8]], wh: f64) -> (Vec<u8>, Vec<u8>) {
        let figures = || EnergyFigures {
            energy_wh: wh,
            energy_cost_usd: 0.0,
            co2_g: 0.0,
        };
        let mut usage = UsageRewriter::new("text/event-stream", strip, stamp).unwrap();
        let (mut client, mut plain) = (Vec::new(), Vec::new());
        for chunk in chunks {
            let out = usage.push(Bytes::copy_from_slice(chunk), figures);
            client.extend_from_slice(&out.client);
            plain.extend_from_slice(&out.unstamped);
        }
        let out = usage.finish(figures);
        client.extend_from_slice(&out.client);
        plain.extend_from_slice(&out.unstamped);
        (client, plain)
    }

    #[test]
    fn recorded_streams_pass_byte_for_byte_but_for_the_usage_event() {
        for wire in recorded() {
            let bytes = wire.as_bytes();
            let usage_free: String = events(&[bytes])
                .into_iter()
                .filter(|e| !e.contains("\"choices\":[],\"usage\""))
                .collect();
            for cut in 0..=bytes.len() {
                let chunks = [&bytes[..cut], &bytes[cut..]];
                // Nothing measured: stamping leaves every byte.
                let (client, plain) = rewrite(false, true, &chunks, 0.0);
                assert_eq!(client, bytes, "cut at {cut}");
                assert_eq!(plain, bytes);
                // Stripping drops the usage event and nothing else.
                let (client, plain) = rewrite(true, false, &chunks, 0.0);
                assert_eq!(
                    String::from_utf8(client).unwrap(),
                    usage_free,
                    "cut at {cut}"
                );
                assert_eq!(String::from_utf8(plain).unwrap(), usage_free);
                // Measured: only the usage event changes, and the cache's copy
                // is the upstream's.
                let (client, plain) = rewrite(false, true, &chunks, 0.25);
                assert_eq!(plain, bytes, "cut at {cut}");
                let client = String::from_utf8(client).unwrap();
                if wire.contains("prompt_tokens") {
                    assert!(client.contains("\"energy_wh\":0.25"), "{client}");
                    assert_eq!(
                        client.replace(",\"energy_wh\":0.25", "").len(),
                        // The stamped event is written again from its JSON;
                        // only the added field changes its length.
                        wire.len(),
                        "{client}"
                    );
                } else {
                    assert_eq!(client, wire);
                }
            }
        }
    }

    #[test]
    fn whole_events_without_usage_pass_without_a_copy() {
        let mut usage = UsageRewriter::new("text/event-stream", true, true).unwrap();
        let chunk =
            Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n");
        let out = usage.push(chunk.clone(), EnergyFigures::default);
        assert_eq!(out.client.as_ptr(), chunk.as_ptr());
    }

    #[test]
    fn nothing_to_do_needs_no_rewriter() {
        assert!(UsageRewriter::new("text/event-stream", false, false).is_none());
        assert!(UsageRewriter::new("application/json", true, false).is_none());
        assert!(UsageRewriter::new("audio/mpeg", false, true).is_none());
        assert!(UsageRewriter::new("application/json", false, true).is_some());
        // Stripping reads any body as events, as the gateway asked for usage
        // whatever the upstream labels its stream.
        assert!(UsageRewriter::new("", true, false).is_some());
    }
}
