use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::export::load_turns;
use crate::scan::{list_projects, resolve, session_files};
use crate::stats::analyze_session;
use crate::style;

pub struct Opts {
    pub claude_dir: PathBuf,
    pub keyword: String,
    /// Only sessions of this directory and below; `None` searches everything.
    pub path: Option<PathBuf>,
    pub ignore_case: bool,
    /// Stop after this many matching messages.
    pub limit: usize,
}

/// Char index of the first occurrence of `kw` in `line`.
fn find(line: &str, kw: &str, ignore_case: bool) -> Option<usize> {
    let byte = if ignore_case { line.to_lowercase().find(&kw.to_lowercase()).map(|b| (b, true)) } else { line.find(kw).map(|b| (b, false)) };
    let (b, lowered) = byte?;
    Some(if lowered { line.to_lowercase()[..b].chars().count() } else { line[..b].chars().count() })
}

/// The first matching line of `text`, cut down around the match: (before, matched, after).
pub fn snippet(text: &str, kw: &str, ignore_case: bool) -> Option<(String, String, String)> {
    let (line, idx) = text.lines().find_map(|l| find(l, kw, ignore_case).map(|i| (l, i)))?;
    let chars: Vec<char> = line.chars().collect();
    let klen = kw.chars().count();
    let (start, mid_end, end) = (idx.saturating_sub(40), (idx + klen).min(chars.len()), (idx + klen + 60).min(chars.len()));
    let s = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
    let before = if start > 0 { format!("…{}", s(start, idx)) } else { s(start, idx) };
    let after = if end < chars.len() { format!("{}…", s(mid_end, end)) } else { s(mid_end, end) };
    Some((before, s(idx, mid_end), after))
}

pub fn run(o: &Opts) -> Result<()> {
    if o.keyword.is_empty() {
        bail!("nothing to search for");
    }
    let base = o.path.as_deref().map(resolve).transpose()?;
    let (mut hits, mut sessions_hit) = (0, 0);
    'outer: for p in list_projects(&o.claude_dir)? {
        if base.as_ref().is_some_and(|b| !p.cwd.starts_with(b)) {
            continue;
        }
        for f in session_files(&p.dir) {
            let mut lines = Vec::new();
            for t in load_turns(&f) {
                let Some((before, mid, after)) = snippet(&t.text, &o.keyword, o.ignore_case) else { continue };
                let who = if t.role == "user" { "user" } else { "assistant" };
                let when = t.timestamp.as_deref().map(|s| s.chars().take(16).collect::<String>().replace('T', " ")).unwrap_or_default();
                lines.push(format!(
                    "  {} {}  {}{}{}",
                    style::dim(&format!("{who:<9}")),
                    style::dim(&when),
                    before.replace('\t', " "),
                    style::bold_yellow(&mid),
                    after.replace('\t', " ")
                ));
                hits += 1;
                if hits >= o.limit {
                    break;
                }
            }
            if !lines.is_empty() {
                sessions_hit += 1;
                let s = analyze_session(&f);
                println!(
                    "{}  {}  {}",
                    style::yellow(&s.id[..s.id.len().min(8)]),
                    style::bold(s.title.as_deref().unwrap_or("(untitled)")),
                    style::cyan(&p.cwd.display().to_string())
                );
                for l in lines {
                    println!("{l}");
                }
            }
            if hits >= o.limit {
                println!("{}", style::dim(&format!("(stopped at {} matches; raise --limit to see more)", o.limit)));
                break 'outer;
            }
        }
    }
    if hits == 0 {
        println!("{}", style::dim("(no matches)"));
    } else if hits < o.limit {
        println!("{}", style::dim(&format!("{hits} match(es) in {sessions_hit} session(s)")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_finds_match_and_trims_context() {
        let (b, m, a) = snippet("first line\nthe Quick brown fox", "quick", true).unwrap();
        assert_eq!((b.as_str(), m.as_str(), a.as_str()), ("the ", "Quick", " brown fox"));
        assert!(snippet("the Quick brown fox", "quick", false).is_none());
        let long = format!("{}needle{}", "a".repeat(100), "b".repeat(100));
        let (b, m, a) = snippet(&long, "needle", false).unwrap();
        assert!(b.starts_with('…') && a.ends_with('…') && m == "needle");
        let (b, m, _) = snippet("找到這個關鍵字在這裡", "關鍵字", false).unwrap();
        assert_eq!((b.as_str(), m.as_str()), ("找到這個", "關鍵字"));
    }
}
