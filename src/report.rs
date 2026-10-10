use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::scan::{is_excluded, list_all_projects, session_files, strip_prefix_ci};
use crate::stats::{Agg, SessionStat, analyze_session};
use crate::style;

#[derive(Serialize)]
pub struct ProjectReport {
    pub path: String,
    #[serde(flatten)]
    pub total: Agg,
    /// Files in the project's auto-memory folder.
    pub memory_files: usize,
    pub session_list: Vec<SessionStat>,
}

#[derive(Serialize)]
pub struct Report {
    pub base: String,
    pub projects: Vec<ProjectReport>,
    pub total: Agg,
    pub memory_files: usize,
}

/// Collects stats for `base` and every project folder below it, skipping any whose
/// path (relative to `base`) passes through an excluded directory name.
pub fn build(claude_dir: &Path, base: &Path, excludes: &[String]) -> Result<Report> {
    collect(claude_dir, base.display().to_string(), |cwd| {
        let rel = strip_prefix_ci(cwd, base)?;
        if is_excluded(&rel, excludes) {
            return None;
        }
        Some(if rel.as_os_str().is_empty() { ".".to_string() } else { rel.display().to_string() })
    })
}

/// Like `build`, but for every directory Claude Code has ever run in on this machine.
/// Paths are shown absolute with the home directory written as `~`; `excludes` skips
/// any project whose path passes through one of those names.
pub fn build_all(claude_dir: &Path, excludes: &[String]) -> Result<Report> {
    let home = dirs::home_dir();
    collect(claude_dir, "all directories".to_string(), |cwd| {
        if is_excluded(cwd, excludes) {
            return None;
        }
        Some(tilde(cwd, home.as_deref()))
    })
}

/// `path` with a leading `home` replaced by `~`.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

/// `label` maps a project's directory to its display path, or `None` to skip it.
fn collect(claude_dir: &Path, base: String, label: impl Fn(&Path) -> Option<String>) -> Result<Report> {
    let mut projects = Vec::new();
    let mut total = Agg::default();
    for p in list_all_projects(claude_dir)? {
        let Some(path) = label(&p.cwd) else { continue };
        let memory_files = crate::memory::count(&p.dir);
        let mut agg = Agg::default();
        let mut list = Vec::new();
        for f in session_files(&p.dir) {
            let s = analyze_session(&f);
            agg.add(&s);
            total.add(&s);
            list.push(s);
        }
        if list.is_empty() && memory_files == 0 {
            continue;
        }
        projects.push(ProjectReport { path, total: agg, memory_files, session_list: list });
    }
    projects.sort_by(|a, b| a.path.cmp(&b.path));
    let memory_files = projects.iter().map(|p| p.memory_files).sum();
    Ok(Report { base, projects, total, memory_files })
}

#[derive(Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SortKey {
    /// By directory (default)
    #[default]
    Path,
    /// Biggest on disk first
    Size,
    /// Most input + output tokens first
    Tokens,
    /// Most messages first
    Messages,
    /// Most recently used first
    LastUsed,
}

/// Post-processing of a built report.
#[derive(Default)]
pub struct View {
    /// Keep only sessions last active at or after this ISO timestamp.
    pub since: Option<String>,
    pub sort: SortKey,
    pub limit: Option<usize>,
}

fn agg_of(sessions: &[SessionStat]) -> Agg {
    let mut a = Agg::default();
    for s in sessions {
        a.add(s);
    }
    a
}

fn by_key(k: SortKey, a: &Agg, b: &Agg) -> std::cmp::Ordering {
    let tokens = |a: &Agg| a.usage.input + a.usage.output;
    match k {
        SortKey::Path => std::cmp::Ordering::Equal,
        SortKey::Size => b.bytes.cmp(&a.bytes),
        SortKey::Tokens => tokens(b).cmp(&tokens(a)),
        SortKey::Messages => b.messages.cmp(&a.messages),
        SortKey::LastUsed => b.last.cmp(&a.last),
    }
}

/// Applies `--since`, `--sort` and `--limit`, then recomputes every total.
pub fn apply(r: &mut Report, v: &View) {
    if let Some(cut) = &v.since {
        for p in &mut r.projects {
            p.session_list.retain(|s| s.last.as_deref().is_some_and(|l| l >= cut.as_str()));
        }
        r.projects.retain(|p| !p.session_list.is_empty());
    }
    for p in &mut r.projects {
        p.total = agg_of(&p.session_list);
        if v.sort != SortKey::Path {
            p.session_list.sort_by(|a, b| by_key(v.sort, &agg_of(std::slice::from_ref(a)), &agg_of(std::slice::from_ref(b))));
        }
    }
    if v.sort != SortKey::Path {
        r.projects.sort_by(|a, b| by_key(v.sort, &a.total, &b.total));
    }
    if let Some(n) = v.limit {
        r.projects.truncate(n);
    }
    r.memory_files = r.projects.iter().map(|p| p.memory_files).sum();
    let mut total = Agg::default();
    for p in &r.projects {
        for s in &p.session_list {
            total.add(s);
        }
    }
    r.total = total;
}

fn human(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

fn ts(t: &Option<String>) -> String {
    t.as_deref().map(|s| s.chars().take(16).collect::<String>().replace('T', " ")).unwrap_or_else(|| "-".into())
}

/// Pads to `w` terminal cells (CJK characters count as two).
fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(s.width())))
}

/// Cuts `s` to at most `w` terminal cells, ending in `…` when anything was dropped.
fn truncate(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

/// Cells are padded before being colored so ANSI codes don't break alignment.
fn row(name: String, a: &Agg, memory: usize, bold: bool) -> String {
    let num = |s: String| if bold { style::bold_green(&s) } else { s };
    let soft = |s: String| if bold { style::bold_green(&s) } else { style::dim(&s) };
    let mem = format!("{:>4}", if memory > 0 { memory.to_string() } else { "-".into() });
    format!(
        "{} {} {} {} {} {} {} {}  {}",
        name,
        num(format!("{:>8}", a.sessions)),
        if bold { style::bold_green(&mem) } else if memory > 0 { style::yellow(&mem) } else { style::dim(&mem) },
        num(format!("{:>8}", human(a.messages))),
        num(format!("{:>9}", human(a.usage.input))),
        num(format!("{:>9}", human(a.usage.output))),
        soft(format!("{:>9}", human(a.usage.cache_read))),
        soft(format!("{:>9}", human(a.usage.cache_write))),
        soft(ts(&a.last))
    )
}

fn kv(key: &str, value: String) {
    println!("{} {}", style::cyan(&format!("{key:<7}")), value);
}

pub fn print_text(r: &Report, detail: bool) {
    println!("{} {}", style::bold_cyan("Claude sessions under"), style::bold(&r.base));
    if r.projects.is_empty() {
        println!("{}", style::dim("(no sessions found)"));
        return;
    }
    let w = r.projects.iter().map(|p| p.path.width()).max().unwrap_or(3).max(5);
    let header = format!(
        "{} {:>8} {:>4} {:>8} {:>9} {:>9} {:>9} {:>9}  {}",
        pad("DIR", w), "SESSIONS", "MEM", "MSGS", "INPUT", "OUTPUT", "CACHE-R", "CACHE-W", "LAST (UTC)"
    );
    let rule = style::dim(&"─".repeat(header.width()));
    println!("\n{}\n{rule}", style::bold(&header));
    for p in &r.projects {
        println!("{}", row(style::cyan(&pad(&p.path, w)), &p.total, p.memory_files, false));
        if detail {
            for s in &p.session_list {
                let title = match s.title.as_deref() {
                    Some(t) => pad(&truncate(t, 40), 40),
                    None => style::dim(&pad("(untitled)", 40)),
                };
                println!(
                    "  {} {}  {}  {}  {}",
                    style::dim("└"),
                    style::yellow(&s.id[..s.id.len().min(8)]),
                    title,
                    style::dim(&format!("msgs {:>5}  out {:>8}", s.messages, human(s.usage.output))),
                    style::dim(&format!("{} → {}", ts(&s.first), ts(&s.last)))
                );
            }
        }
    }
    println!("{rule}");
    println!("{}", row(style::bold_green(&pad("TOTAL", w)), &r.total, r.memory_files, true));
    println!();
    kv("Range", format!("{} → {} {}", ts(&r.total.first), ts(&r.total.last), style::dim("(UTC)")));
    kv("Size", format!("{:.1} MB on disk", r.total.bytes as f64 / 1_048_576.0));
    let mut models: Vec<_> = r.total.models.iter().collect();
    models.sort_by(|a, b| b.1.cmp(a.1));
    let ms: Vec<String> = models.iter().map(|(m, n)| format!("{} {}", style::bold(m), style::dim(&format!("×{n}")))).collect();
    kv("Models", if ms.is_empty() { "-".into() } else { ms.join("  ") });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_counts_cjk_as_two_cells() {
        assert_eq!(pad("提交並推送修復", 40).width(), 40);
        assert_eq!(pad("abc", 10).width(), 10);
    }

    fn sess(id: &str, last: &str, messages: u64, bytes: u64) -> SessionStat {
        SessionStat { id: id.into(), last: Some(last.into()), messages, bytes, ..Default::default() }
    }

    fn proj(path: &str, list: Vec<SessionStat>) -> ProjectReport {
        ProjectReport { path: path.into(), total: Agg::default(), memory_files: 0, session_list: list }
    }

    #[test]
    fn apply_filters_sorts_limits_and_recomputes_totals() {
        let mut r = Report {
            base: "/".into(),
            projects: vec![
                proj("a", vec![sess("a1", "2026-01-01T00:00:00Z", 5, 10), sess("a2", "2026-03-01T00:00:00Z", 1, 1)]),
                proj("b", vec![sess("b1", "2026-02-01T00:00:00Z", 9, 100)]),
                proj("c", vec![sess("c1", "2025-01-01T00:00:00Z", 50, 1000)]),
            ],
            total: Agg::default(),
            memory_files: 0,
        };
        let v = View { since: Some("2026-01-15T00:00:00Z".into()), sort: SortKey::Size, limit: Some(1) };
        apply(&mut r, &v);
        assert_eq!(r.projects.len(), 1);
        assert_eq!(r.projects[0].path, "b");
        assert_eq!(r.total.sessions, 1);
        assert_eq!(r.total.messages, 9);

        let mut r2 = Report {
            base: "/".into(),
            projects: vec![proj("a", vec![sess("a1", "2026-01-01T00:00:00Z", 5, 10), sess("a2", "2026-03-01T00:00:00Z", 1, 1)])],
            total: Agg::default(),
            memory_files: 0,
        };
        apply(&mut r2, &View { sort: SortKey::LastUsed, ..Default::default() });
        assert_eq!(r2.projects[0].session_list[0].id, "a2");
        assert_eq!((r2.total.sessions, r2.total.messages), (2, 6));
    }

    #[test]
    fn tilde_abbreviates_only_paths_under_home() {
        let home = Path::new("/home/u");
        assert_eq!(tilde(Path::new("/home/u"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/home/u/p/x"), Some(home)), "~/p/x");
        assert_eq!(tilde(Path::new("/home/user2/p"), Some(home)), "/home/user2/p");
        assert_eq!(tilde(Path::new("/opt/p"), None), "/opt/p");
    }

    #[test]
    fn build_all_lists_unrelated_directories_and_honors_excludes() {
        let t = tempfile::tempdir().unwrap();
        let claude = t.path().join(".claude");
        for (name, cwd) in [("-a-one", "/a/one"), ("-b-two", "/b/node_modules/two")] {
            let d = claude.join("projects").join(name);
            std::fs::create_dir_all(&d).unwrap();
            let line = format!("{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n");
            std::fs::write(d.join("s1.jsonl"), line).unwrap();
        }
        let r = build_all(&claude, &[]).unwrap();
        assert_eq!(r.projects.len(), 2);
        assert_eq!(r.total.sessions, 2);
        let r = build_all(&claude, &["node_modules".to_string()]).unwrap();
        assert_eq!(r.projects.len(), 1);
        assert_eq!(r.projects[0].path, "/a/one");
    }

    #[test]
    fn memory_is_counted_and_memory_only_projects_are_listed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let claude = root.join(".claude");
        let (with_sessions, memory_only) = (root.join("p/a"), root.join("p/b"));
        std::fs::create_dir_all(&memory_only).unwrap();
        for (cwd, sessions) in [(&with_sessions, true), (&memory_only, false)] {
            let d = claude.join("projects").join(crate::encode::encode_path(cwd));
            std::fs::create_dir_all(d.join("memory")).unwrap();
            std::fs::write(d.join("memory/MEMORY.md"), "- [x](x.md)\n").unwrap();
            std::fs::write(d.join("memory/x.md"), "x\n").unwrap();
            if sessions {
                let c = serde_json::to_string(&cwd.display().to_string()).unwrap();
                std::fs::write(d.join("s.jsonl"), format!("{{\"cwd\":{c},\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n")).unwrap();
            }
        }
        let r = build(&claude, &root.join("p"), &[]).unwrap();
        let rows: Vec<(&str, usize, u64)> = r.projects.iter().map(|p| (p.path.as_str(), p.memory_files, p.total.sessions)).collect();
        assert_eq!(rows, [("a", 2, 1), ("b", 2, 0)]);
        assert_eq!(r.memory_files, 4);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["projects"][1]["memory_files"], 2);
    }

    #[test]
    fn truncate_respects_display_width() {
        let t = truncate("提交並推送修復提交並推送修復提交並推送修復", 40);
        assert!(t.width() <= 40 && t.ends_with('…'));
        assert_eq!(truncate("short", 40), "short");
    }
}
