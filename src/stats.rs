use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

#[derive(Default, Serialize, Clone, Copy)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
}

#[derive(Default, Serialize)]
pub struct SessionStat {
    pub id: String,
    pub title: Option<String>,
    pub messages: u64,
    pub usage: Usage,
    pub first: Option<String>,
    pub last: Option<String>,
    pub models: BTreeMap<String, u64>,
    pub bytes: u64,
}

/// Running totals over any number of sessions.
#[derive(Default, Serialize)]
pub struct Agg {
    pub sessions: u64,
    pub messages: u64,
    pub usage: Usage,
    pub first: Option<String>,
    pub last: Option<String>,
    pub models: BTreeMap<String, u64>,
    pub bytes: u64,
}

impl Agg {
    pub fn add(&mut self, s: &SessionStat) {
        self.sessions += 1;
        self.messages += s.messages;
        self.usage.add(&s.usage);
        self.bytes += s.bytes;
        widen(&mut self.first, &mut self.last, s.first.as_deref(), s.last.as_deref());
        for (m, n) in &s.models {
            *self.models.entry(m.clone()).or_default() += n;
        }
    }
}

fn widen(first: &mut Option<String>, last: &mut Option<String>, f: Option<&str>, l: Option<&str>) {
    if let Some(f) = f {
        if first.as_deref().is_none_or(|c| f < c) {
            *first = Some(f.to_string());
        }
    }
    if let Some(l) = l {
        if last.as_deref().is_none_or(|c| l > c) {
            *last = Some(l.to_string());
        }
    }
}

fn is_tool_result_only(content: &Value) -> bool {
    match content.as_array() {
        Some(blocks) => {
            !blocks.is_empty()
                && blocks.iter().all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        }
        None => false,
    }
}

/// Streams one session file. Unparseable lines are skipped. Assistant turns are
/// written as several lines sharing one message id, so they are de-duplicated by id.
pub fn analyze_session(path: &Path) -> SessionStat {
    let mut s = SessionStat {
        id: path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        bytes: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ..Default::default()
    };
    let Ok(file) = File::open(path) else { return s };
    let mut seen = HashSet::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(ts) = v.get("timestamp").and_then(Value::as_str) {
            let (mut f, mut l) = (s.first.take(), s.last.take());
            widen(&mut f, &mut l, Some(ts), Some(ts));
            (s.first, s.last) = (f, l);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(Value::as_str) {
                    s.title = Some(t.to_string());
                }
            }
            Some("user") => {
                let content = v.pointer("/message/content").unwrap_or(&Value::Null);
                if !is_tool_result_only(content) {
                    s.messages += 1;
                }
            }
            Some("assistant") => {
                let msg = &v["message"];
                let key = msg.get("id").and_then(Value::as_str).map(str::to_string)
                    .or_else(|| v.get("uuid").and_then(Value::as_str).map(str::to_string));
                if let Some(k) = key {
                    if !seen.insert(k) {
                        continue;
                    }
                }
                s.messages += 1;
                if let Some(m) = msg.get("model").and_then(Value::as_str) {
                    if m != "<synthetic>" {
                        *s.models.entry(m.to_string()).or_default() += 1;
                    }
                }
                let u = &msg["usage"];
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                s.usage.input += n("input_tokens");
                s.usage.output += n("output_tokens");
                s.usage.cache_read += n("cache_read_input_tokens");
                s.usage.cache_write += n("cache_creation_input_tokens");
            }
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedups_assistant_lines_and_skips_tool_results() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("abc.jsonl");
        let lines = [
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hi"}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"x","usage":{"input_tokens":2,"output_tokens":5,"cache_read_input_tokens":7,"cache_creation_input_tokens":11}}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m1","model":"x","usage":{"input_tokens":2,"output_tokens":5,"cache_read_input_tokens":7,"cache_creation_input_tokens":11}}}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:03Z","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"ai-title","aiTitle":"T"}"#,
            "not json",
        ];
        fs::write(&f, lines.join("\n")).unwrap();
        let s = analyze_session(&f);
        assert_eq!(s.id, "abc");
        assert_eq!(s.messages, 2);
        assert_eq!(s.usage.output, 5);
        assert_eq!(s.usage.cache_write, 11);
        assert_eq!(s.title.as_deref(), Some("T"));
        assert_eq!(s.first.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(s.last.as_deref(), Some("2026-01-01T00:00:03Z"));
    }
}
