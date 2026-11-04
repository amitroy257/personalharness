//! Drive an upstream response to completion: forward raw bytes (API
//! passthrough) and/or parsed deltas (chat UI), and collect usage metrics.

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;

use crate::sse::{chunk_info, completion_to_sse, ChunkInfo, SseParser};
use crate::store::UsageRecord;

#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub content: String,
    pub reasoning: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub duration_ms: i64,
    pub error: Option<String>,
    /// Consumer stopped early (client disconnected or user pressed stop).
    pub stopped: bool,
}

impl Outcome {
    pub fn usage_record(&self, c: &crate::router::Candidate, source: &str) -> UsageRecord {
        let est = |s: &str| ((s.chars().count() as f64) / 4.0).ceil() as i64;
        UsageRecord {
            provider: c.entry.provider.clone(),
            model: c.entry.model.clone(),
            family: c.entry.family.clone(),
            source: source.into(),
            ok: self.error.is_none(),
            status: Some(200),
            error: self.error.clone(),
            ttft_ms: self.ttft_ms,
            duration_ms: Some(self.duration_ms),
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens.or_else(|| {
                let n = est(&self.content) + est(&self.reasoning);
                (n > 0).then_some(n)
            }),
        }
    }
}

const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Pump a (possibly streaming) upstream response.
///
/// * `on_raw`  receives SSE bytes to forward; return `false` to stop.
/// * `on_info` receives each parsed chunk; return `false` to stop.
pub async fn pump(
    resp: reqwest::Response,
    started: Instant,
    mut on_raw: impl FnMut(Bytes) -> bool,
    mut on_info: impl FnMut(&ChunkInfo) -> bool,
) -> Outcome {
    let mut out = Outcome::default();
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("event-stream"))
        .unwrap_or(false);

    if !is_sse {
        // Provider ignored `stream: true`; synthesize a stream from the JSON body.
        match resp.bytes().await {
            Ok(b) => {
                out.ttft_ms = Some(started.elapsed().as_millis() as i64);
                match serde_json::from_slice::<Value>(&b) {
                    Ok(v) => {
                        let sse = completion_to_sse(&v);
                        let mut p = SseParser::default();
                        for payload in p.push(sse.as_bytes()) {
                            let info = chunk_info(&payload);
                            absorb(&mut out, &info);
                            if !on_info(&info) {
                                out.stopped = true;
                            }
                        }
                        on_raw(Bytes::from(sse));
                    }
                    Err(e) => out.error = Some(format!("unreadable response: {e}")),
                }
            }
            Err(e) => out.error = Some(e.to_string()),
        }
        out.duration_ms = started.elapsed().as_millis() as i64;
        return out;
    }

    let mut parser = SseParser::default();
    let mut stream = resp.bytes_stream();
    'outer: loop {
        let next = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Err(_) => {
                out.error = Some("provider stopped sending data (idle timeout)".into());
                break;
            }
            Ok(n) => n,
        };
        match next {
            None => break,
            Some(Err(e)) => {
                out.error = Some(format!("stream interrupted: {e}"));
                break;
            }
            Some(Ok(chunk)) => {
                if !on_raw(chunk.clone()) {
                    out.stopped = true;
                    break;
                }
                for payload in parser.push(&chunk) {
                    let info = chunk_info(&payload);
                    if out.ttft_ms.is_none() && (!info.content.is_empty() || !info.reasoning.is_empty()) {
                        out.ttft_ms = Some(started.elapsed().as_millis() as i64);
                    }
                    absorb(&mut out, &info);
                    if !on_info(&info) {
                        out.stopped = true;
                        break 'outer;
                    }
                    if info.done {
                        break 'outer;
                    }
                }
            }
        }
    }
    for payload in parser.finish() {
        let info = chunk_info(&payload);
        absorb(&mut out, &info);
        on_info(&info);
    }
    out.duration_ms = started.elapsed().as_millis() as i64;
    out
}

fn absorb(out: &mut Outcome, info: &ChunkInfo) {
    out.content.push_str(&info.content);
    out.reasoning.push_str(&info.reasoning);
    if info.prompt_tokens.is_some() {
        out.prompt_tokens = info.prompt_tokens;
    }
    if info.completion_tokens.is_some() {
        out.completion_tokens = info.completion_tokens;
    }
    if let Some(e) = &info.error {
        out.error = Some(e.clone());
    }
}
