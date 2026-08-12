//! Reading a streamed response, frame by frame.
//!
//! Two framings cover every endpoint seen so far, and the difference is one line
//! prefix:
//!
//! * **SSE** — `text/event-stream`, frames separated by a blank line, payload on
//!   `data:` lines, and `[DONE]` as an end sentinel. `OpenAI` and everything that
//!   imitates it.
//! * **NDJSON** — one JSON object per line, no prefix and no sentinel. Ollama's
//!   native `/api/chat`, and most things that predate the `OpenAI` shape.
//!
//! Which one is in use is **detected, not declared**. The endpoint already says
//! so in its `content-type`, and when it does not, the first line does: a profile
//! knob here would be a question `mire` can answer by looking.
//!
//! Nothing in this module fails. A frame that will not parse is counted and
//! carried forward — a stream that goes wrong halfway is a finding, and the
//! chunks that did arrive are the evidence.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::paths::{self, resolve};
use super::{DecodeField, DecodeTrace};
use crate::profile::DecodeSpec;

/// How the endpoint delimits its chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// `text/event-stream`: blank-line-separated frames, `data:` payloads.
    Sse,
    /// One JSON value per line.
    Ndjson,
}

impl Framing {
    /// Picks a framing from the response's `content-type`.
    ///
    /// Anything that is not explicitly an event stream is read as NDJSON, which
    /// is the more forgiving of the two: a single JSON object arriving on one
    /// line parses as one chunk, so a profile that asks for a stream and gets an
    /// ordinary answer still decodes.
    #[must_use]
    pub fn detect(content_type: Option<&str>) -> Self {
        match content_type {
            Some(value) if value.to_ascii_lowercase().contains("text/event-stream") => Self::Sse,
            _ => Self::Ndjson,
        }
    }
}

/// What one frame turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// A chunk that parsed.
    Chunk(Box<Value>),
    /// The `[DONE]` sentinel: the endpoint says it is finished.
    Done,
    /// A frame that did not parse as JSON, kept as text.
    Unparsable(String),
}

/// Splits a byte stream into frames.
///
/// Fed arbitrary slices — a chunk boundary lands mid-frame often enough that
/// pretending otherwise is how streaming clients lose the last token of every
/// other message.
#[derive(Debug)]
pub struct FrameParser {
    framing: Framing,
    buffer: String,
}

impl FrameParser {
    /// Starts a parser for the given framing.
    #[must_use]
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buffer: String::new(),
        }
    }

    /// Feeds more bytes and returns whatever complete frames that produced.
    pub fn push(&mut self, text: &str) -> Vec<Frame> {
        self.buffer.push_str(text);
        match self.framing {
            Framing::Sse => self.drain(next_sse_frame),
            Framing::Ndjson => self.drain(next_line),
        }
    }

    /// Returns whatever is left once the connection closes.
    ///
    /// An endpoint that ends without a trailing newline still sent that last
    /// chunk, and it is usually the one carrying `finish_reason`.
    pub fn finish(&mut self) -> Vec<Frame> {
        let rest = std::mem::take(&mut self.buffer);
        let payload = match self.framing {
            Framing::Sse => sse_payload(&rest),
            Framing::Ndjson => rest.trim().to_owned(),
        };
        parse(&payload).into_iter().collect()
    }

    fn drain(&mut self, mut next: impl FnMut(&str) -> Option<(String, usize)>) -> Vec<Frame> {
        let mut frames = Vec::new();
        while let Some((payload, consumed)) = next(&self.buffer) {
            self.buffer.drain(..consumed);
            frames.extend(parse(&payload));
        }
        frames
    }
}

/// Turns one frame's payload into a [`Frame`], or nothing when it is empty.
///
/// Empty payloads are the norm, not an edge case: SSE comment lines, keep-alives
/// and the blank line before the first chunk all land here.
fn parse(payload: &str) -> Option<Frame> {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "[DONE]" {
        return Some(Frame::Done);
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => Some(Frame::Chunk(Box::new(value))),
        Err(_) => Some(Frame::Unparsable(trimmed.to_owned())),
    }
}

/// Finds the next `\n\n`-terminated frame and returns its `data:` payload.
fn next_sse_frame(buffer: &str) -> Option<(String, usize)> {
    let (end, width) = match (buffer.find("\n\n"), buffer.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if crlf < lf => (crlf, 4),
        (Some(lf), _) => (lf, 2),
        (None, Some(crlf)) => (crlf, 4),
        (None, None) => return None,
    };
    Some((sse_payload(&buffer[..end]), end + width))
}

/// Concatenates a frame's `data:` lines, ignoring everything else.
///
/// `event:`, `id:` and `retry:` are dropped on purpose. A chat stream's event
/// names carry no information the payload does not, and inventing a meaning for
/// them here would be guessing on the endpoint's behalf.
fn sse_payload(frame: &str) -> String {
    frame
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("data:")?;
            // `data: x` and `data:x` are the same field; exactly one leading
            // space is part of the framing, any others are payload.
            Some(rest.strip_prefix(' ').unwrap_or(rest))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Finds the next newline-terminated line.
fn next_line(buffer: &str) -> Option<(String, usize)> {
    let end = buffer.find('\n')?;
    Some((buffer[..end].trim_end_matches('\r').to_owned(), end + 1))
}

/// Reads the text delta out of one chunk.
///
/// Same cascade machinery as every other decoded field: paths are tried in
/// order, the first that resolves wins. A chunk that resolves to an empty string
/// — the role-only first chunk of an `OpenAI` stream, or the final chunk that
/// carries only `finish_reason` — is not a delta and does not count as one.
///
/// The trace is only touched on a hit: a stream has hundreds of chunks, and
/// recording a miss per chunk would bury the trace under the one fact it already
/// knows.
#[must_use]
pub fn delta(chunk: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Option<String> {
    let (path, nodes) = resolve(chunk, &spec.delta)?;

    let mut text = String::new();
    for node in &nodes {
        match node {
            Value::String(part) => text.push_str(part),
            // A number or an object here means the path points at the wrong
            // thing, which is worth saying once.
            other => {
                // Once, not once per chunk: the path is wrong for the whole
                // stream, and five hundred copies of that sentence is not five
                // hundred times as useful.
                let known = trace
                    .issues
                    .iter()
                    .any(|issue| issue.field == DecodeField::Delta);
                if !known {
                    trace.issue(
                        DecodeField::Delta,
                        path.source(),
                        format!("expected a string, found {}", super::chat::type_name(other)),
                    );
                }
                return None;
            }
        }
    }

    if text.is_empty() {
        return None;
    }
    trace.hit(DecodeField::Delta, path.source());
    Some(text)
}

/// Records that no configured delta path ever resolved.
///
/// Called once at the end rather than per chunk, and only when the profile
/// actually asked for something.
pub fn record_miss(spec: &DecodeSpec, trace: &mut DecodeTrace) {
    if !trace.matched.contains_key(&DecodeField::Delta) {
        trace.miss(DecodeField::Delta, paths::sources(&spec.delta));
    }
}

/// What the stream itself did, as opposed to what it said.
///
/// This is the half of a streaming test that a non-streaming call cannot answer:
/// whether chunks actually arrived separately, how many, and whether the endpoint
/// ended the stream or the connection just stopped.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamView {
    /// How the chunks were delimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framing: Option<Framing>,
    /// Frames that parsed as JSON.
    pub chunks: u64,
    /// Chunks that carried text. Lower than `chunks` for any endpoint that opens
    /// with a role and closes with a finish reason.
    pub deltas: u64,
    /// Frames that did not parse. Anything above zero is worth a look.
    pub unparsable: u64,
    /// Bytes read off the wire.
    pub bytes: u64,
    /// Whether the endpoint ended the stream itself — a `[DONE]` sentinel, or a
    /// final chunk saying so. `false` means the connection simply stopped, which
    /// is what a proxy cutting the stream looks like.
    pub terminated: bool,
    /// Time to the first frame, whatever it carried. Together with `ttftMs` it
    /// separates "the endpoint is slow to start" from "the endpoint sends a
    /// preamble before it has anything to say".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_chunk_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(frames: Vec<Frame>) -> Vec<Value> {
        frames
            .into_iter()
            .filter_map(|frame| match frame {
                Frame::Chunk(value) => Some(*value),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_framing_comes_from_the_content_type() {
        assert_eq!(
            Framing::detect(Some("text/event-stream; charset=utf-8")),
            Framing::Sse
        );
        assert_eq!(Framing::detect(Some("application/json")), Framing::Ndjson);
        assert_eq!(
            Framing::detect(Some("application/x-ndjson")),
            Framing::Ndjson
        );
        assert_eq!(Framing::detect(None), Framing::Ndjson);
    }

    #[test]
    fn an_sse_stream_splits_on_blank_lines() {
        let mut parser = FrameParser::new(Framing::Sse);
        let frames = parser.push("data: {\"a\":1}\n\ndata: {\"a\":2}\n\n");
        assert_eq!(chunks(frames).len(), 2);
    }

    /// The case that separates a working streaming client from one that drops a
    /// token every few chunks: the network does not respect frame boundaries.
    #[test]
    fn a_frame_split_across_reads_is_reassembled() {
        let mut parser = FrameParser::new(Framing::Sse);
        assert!(parser.push("data: {\"content\":\"hel").is_empty());
        assert!(parser.push("lo\"}").is_empty());
        let frames = parser.push("\n\n");

        assert_eq!(
            chunks(frames),
            vec![serde_json::json!({"content": "hello"})]
        );
    }

    #[test]
    fn the_done_sentinel_is_recognised() {
        let mut parser = FrameParser::new(Framing::Sse);
        let frames = parser.push("data: [DONE]\n\n");
        assert_eq!(frames, vec![Frame::Done]);
    }

    #[test]
    fn sse_comments_and_keep_alives_are_ignored() {
        let mut parser = FrameParser::new(Framing::Sse);
        // A comment frame, then an event name with no data: both are noise.
        let frames = parser.push(": keep-alive\n\nevent: ping\n\n");
        assert!(frames.is_empty(), "{frames:?}");
    }

    #[test]
    fn a_multi_line_data_payload_is_joined() {
        let mut parser = FrameParser::new(Framing::Sse);
        let frames = parser.push("data: {\"a\":\ndata: 1}\n\n");
        assert_eq!(chunks(frames), vec![serde_json::json!({"a": 1})]);
    }

    #[test]
    fn crlf_framing_works_too() {
        let mut parser = FrameParser::new(Framing::Sse);
        let frames = parser.push("data: {\"a\":1}\r\n\r\n");
        assert_eq!(chunks(frames), vec![serde_json::json!({"a": 1})]);
    }

    #[test]
    fn ndjson_is_one_object_per_line() {
        let mut parser = FrameParser::new(Framing::Ndjson);
        let frames = parser.push("{\"a\":1}\n{\"a\":2}\n");
        assert_eq!(chunks(frames).len(), 2);
    }

    /// Ollama ends without a trailing newline often enough, and that last object
    /// is the one carrying `done: true` and the token counts.
    #[test]
    fn the_last_line_survives_a_missing_newline() {
        let mut parser = FrameParser::new(Framing::Ndjson);
        assert!(parser.push("{\"done\":true}").is_empty());
        assert_eq!(
            chunks(parser.finish()),
            vec![serde_json::json!({"done": true})]
        );
    }

    #[test]
    fn a_frame_that_is_not_json_is_kept_rather_than_dropped() {
        let mut parser = FrameParser::new(Framing::Sse);
        let frames = parser.push("data: <html>502 Bad Gateway</html>\n\n");
        assert_eq!(
            frames,
            vec![Frame::Unparsable("<html>502 Bad Gateway</html>".to_owned())]
        );
    }

    fn spec(paths: &[&str]) -> DecodeSpec {
        DecodeSpec {
            delta: paths.iter().map(|p| p.parse().unwrap()).collect(),
            ..DecodeSpec::default()
        }
    }

    #[test]
    fn the_delta_cascade_takes_the_first_path_that_resolves() {
        let spec = spec(&["$.choices[0].delta.content", "$.message.content"]);
        let mut trace = DecodeTrace::default();

        let ollama = serde_json::json!({"message": {"content": "hi"}});
        assert_eq!(delta(&ollama, &spec, &mut trace), Some("hi".to_owned()));
        assert_eq!(
            trace.matched.get(&DecodeField::Delta).map(String::as_str),
            Some("$.message.content")
        );
    }

    /// The first chunk of an `OpenAI` stream announces the role and carries no
    /// text. Counting it as a delta would put time-to-first-token before the
    /// first token.
    #[test]
    fn an_empty_delta_is_not_a_delta() {
        let spec = spec(&["$.choices[0].delta.content"]);
        let mut trace = DecodeTrace::default();
        let chunk = serde_json::json!({"choices": [{"delta": {"content": ""}}]});

        assert_eq!(delta(&chunk, &spec, &mut trace), None);
    }

    #[test]
    fn a_chunk_with_no_delta_at_all_is_silent() {
        let spec = spec(&["$.choices[0].delta.content"]);
        let mut trace = DecodeTrace::default();
        let chunk = serde_json::json!({"choices": [{"finish_reason": "stop"}]});

        assert_eq!(delta(&chunk, &spec, &mut trace), None);
        assert!(trace.issues.is_empty());
        assert!(trace.missed.is_empty());
    }

    #[test]
    fn a_delta_path_pointing_at_the_wrong_kind_is_reported_once() {
        let spec = spec(&["$.choices[0].delta"]);
        let mut trace = DecodeTrace::default();
        let chunk = serde_json::json!({"choices": [{"delta": {"content": "hi"}}]});

        assert_eq!(delta(&chunk, &spec, &mut trace), None);
        assert_eq!(trace.issues.len(), 1);
        // Five hundred chunks must not mean five hundred identical issues.
        let _ = delta(&chunk, &spec, &mut trace);
        assert_eq!(trace.issues.len(), 1);
    }

    #[test]
    fn a_cascade_that_never_resolves_is_recorded_once_at_the_end() {
        let spec = spec(&["$.nope"]);
        let mut trace = DecodeTrace::default();
        record_miss(&spec, &mut trace);

        assert_eq!(
            trace.missed.get(&DecodeField::Delta),
            Some(&vec!["$.nope".to_owned()])
        );
    }
}
