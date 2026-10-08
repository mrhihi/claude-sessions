use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::stats::SessionStat;

/// One message of the conversation as people read it: no thinking, no tool output.
#[derive(Serialize)]
pub struct Turn {
    pub role: &'static str,
    pub timestamp: Option<String>,
    pub model: Option<String>,
    pub text: String,
}

struct Raw {
    role: &'static str,
    id: Option<String>,
    timestamp: Option<String>,
    model: Option<String>,
    parts: Vec<String>,
}

fn clip(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n { s } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

fn tool_line(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = block.get("input");
    let arg = ["command", "file_path", "path", "pattern", "url", "description", "prompt"]
        .iter()
        .find_map(|k| input.and_then(|i| i.get(*k)).and_then(Value::as_str));
    match arg {
        Some(a) => format!("[tool: {name}] {}", clip(a, 120)),
        None => format!("[tool: {name}]"),
    }
}

/// Text of a message's content, or `None` if there is nothing worth showing
/// (tool results, thinking).
fn parts_of(content: &Value) -> Vec<String> {
    match content {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
                Some("tool_use") => Some(tool_line(b)),
                Some("image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Reads the conversation of one session file. Claude Code writes an assistant
/// message as several lines (one per content block) sharing a message id, so those
/// lines are merged into one turn.
pub fn load_turns(path: &Path) -> Vec<Turn> {
    let Ok(file) = File::open(path) else { return vec![] };
    let mut raws: Vec<Raw> = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) || v.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let role = match v.get("type").and_then(Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        let parts = parts_of(v.pointer("/message/content").unwrap_or(&Value::Null));
        if parts.is_empty() {
            continue;
        }
        let id = v.pointer("/message/id").and_then(Value::as_str).map(str::to_string);
        if role == "assistant" {
            if let Some(last) = raws.last_mut() {
                if last.role == "assistant" && last.id.is_some() && last.id == id {
                    last.parts.extend(parts);
                    continue;
                }
            }
        }
        raws.push(Raw {
            role,
            id,
            timestamp: v.get("timestamp").and_then(Value::as_str).map(str::to_string),
            model: v.pointer("/message/model").and_then(Value::as_str).filter(|m| *m != "<synthetic>").map(str::to_string),
            parts,
        });
    }
    raws.into_iter()
        .map(|r| Turn { role: r.role, timestamp: r.timestamp, model: r.model, text: r.parts.join("\n\n") })
        .collect()
}

pub fn to_markdown(stat: &SessionStat, cwd: &Path, turns: &[Turn]) -> String {
    let mut out = format!("# {}\n\n", stat.title.as_deref().unwrap_or("(untitled session)"));
    out.push_str(&format!("- Session: `{}`\n- Directory: `{}`\n", stat.id, cwd.display()));
    if let (Some(f), Some(l)) = (&stat.first, &stat.last) {
        out.push_str(&format!("- Time (UTC): {f} → {l}\n"));
    }
    if !stat.models.is_empty() {
        out.push_str(&format!("- Models: {}\n", stat.models.keys().cloned().collect::<Vec<_>>().join(", ")));
    }
    for t in turns {
        let who = if t.role == "user" { "User" } else { "Assistant" };
        let when = t.timestamp.as_deref().map(|s| format!(" · {}", s.chars().take(19).collect::<String>().replace('T', " "))).unwrap_or_default();
        out.push_str(&format!("\n---\n\n## {who}{when}\n\n{}\n", t.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_assistant_blocks_and_skips_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("s.jsonl");
        let lines = [
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello"}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"x","content":[{"type":"thinking","thinking":"hmm"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m1","model":"x","content":[{"type":"text","text":"let me look"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m1","model":"x","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"big output"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"caveat"}}"#,
            r#"{"type":"assistant","message":{"id":"m2","model":"x","content":[{"type":"text","text":"done"}]}}"#,
            "garbage",
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        let t = load_turns(&f);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].text, "hello");
        assert_eq!(t[1].text, "let me look\n\n[tool: Read] /a/b.rs");
        assert_eq!(t[2].text, "done");
        let md = to_markdown(&SessionStat { id: "s".into(), ..Default::default() }, Path::new("/a"), &t);
        assert!(md.starts_with("# (untitled session)") && md.contains("## User") && md.contains("## Assistant"));
    }
}
