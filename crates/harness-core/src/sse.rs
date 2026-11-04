//! Minimal incremental Server-Sent-Events parser for OpenAI-style streams.

use serde_json::Value;

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    /// Feed raw bytes; returns completed `data:` payloads (multi-line data joined by `\n`).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).to_string();
            self.line(line, &mut out);
        }
        out
    }

    /// Flush a trailing event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).trim_end_matches(['\r', '\n']).to_string();
            self.line(rest, &mut out);
        }
        if !self.data.is_empty() {
            out.push(self.data.join("\n"));
            self.data.clear();
        }
        out
    }

    fn line(&mut self, line: String, out: &mut Vec<String>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(self.data.join("\n"));
                self.data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
        // comments (":"), "event:", "id:", "retry:" are ignored
    }
}

/// What one streamed chunk contributes.
#[derive(Debug, Default, PartialEq)]
pub struct ChunkInfo {
    pub content: String,
    pub reasoning: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub error: Option<String>,
    pub done: bool,
}

pub fn chunk_info(payload: &str) -> ChunkInfo {
    let mut info = ChunkInfo::default();
    let p = payload.trim();
    if p == "[DONE]" {
        info.done = true;
        return info;
    }
    let Ok(v) = serde_json::from_str::<Value>(p) else { return info };
    if let Some(err) = v.get("error") {
        info.error = Some(err.get("message").and_then(|m| m.as_str()).map(String::from).unwrap_or_else(|| err.to_string()));
    }
    if let Some(choice) = v.get("choices").and_then(|c| c.get(0)) {
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(d) = delta {
            if let Some(c) = d.get("content").and_then(|c| c.as_str()) {
                info.content.push_str(c);
            }
            for k in ["reasoning_content", "reasoning"] {
                if let Some(r) = d.get(k).and_then(|c| c.as_str()) {
                    info.reasoning.push_str(r);
                }
            }
        }
    }
    let usage = v.get("usage").filter(|u| !u.is_null()).or_else(|| v.get("x_groq").and_then(|x| x.get("usage")));
    if let Some(u) = usage {
        info.prompt_tokens = u.get("prompt_tokens").and_then(|t| t.as_i64());
        info.completion_tokens = u.get("completion_tokens").and_then(|t| t.as_i64());
    }
    info
}

/// Convert a non-streamed chat completion into SSE (for providers that ignore `stream: true`).
pub fn completion_to_sse(v: &Value) -> String {
    let content = v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("");
    let chunk = serde_json::json!({
        "id": v.get("id").cloned().unwrap_or(Value::Null),
        "object": "chat.completion.chunk",
        "created": v.get("created").cloned().unwrap_or(Value::Null),
        "model": v.get("model").cloned().unwrap_or(Value::Null),
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": v.get("usage").cloned().unwrap_or(Value::Null),
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_across_chunks() {
        let mut p = SseParser::default();
        let a = p.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel");
        assert!(a.is_empty());
        let b = p.push(b"lo\"}}]}\r\n\r\n: keepalive\n\ndata: [DONE]\n\n");
        assert_eq!(b.len(), 2);
        assert_eq!(chunk_info(&b[0]).content, "Hello");
        assert!(chunk_info(&b[1]).done);
        assert!(p.finish().is_empty());
    }

    #[test]
    fn usage_reasoning_errors() {
        let i = chunk_info(
            r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}],"usage":{"prompt_tokens":3,"completion_tokens":7}}"#,
        );
        assert_eq!(i.reasoning, "hmm");
        assert_eq!(i.completion_tokens, Some(7));
        let g = chunk_info(r#"{"choices":[],"x_groq":{"usage":{"prompt_tokens":1,"completion_tokens":2}}}"#);
        assert_eq!(g.completion_tokens, Some(2));
        let e = chunk_info(r#"{"error":{"message":"overloaded"}}"#);
        assert_eq!(e.error.as_deref(), Some("overloaded"));
    }

    #[test]
    fn finish_flushes() {
        let mut p = SseParser::default();
        assert!(p.push(b"data: x").is_empty());
        assert_eq!(p.finish(), vec!["x".to_string()]);
    }
}
