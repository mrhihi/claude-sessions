use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::encode::encode_path;
use crate::scan::{Project, list_projects, projects_root, resolve};
use crate::style;

pub struct Opts {
    pub claude_dir: PathBuf,
    pub src: PathBuf,
    pub dst: PathBuf,
    pub dry_run: bool,
    pub no_move_files: bool,
    pub force: bool,
}

pub(crate) struct Step {
    pub(crate) project: Project,
    pub(crate) new_dir: PathBuf,
}

fn json_escape(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap();
    q[1..q.len() - 1].to_string()
}

/// Replaces `"key":"<old>..."` with `<new>` where `old` is a whole-path prefix.
/// Text-level so the rest of each line keeps its exact bytes and key order.
pub fn rewrite_prefix(text: &str, key: &str, old: &str, new: &str) -> (String, usize) {
    let needle = format!("\"{key}\":\"{}", json_escape(old));
    let rep = format!("\"{key}\":\"{}", json_escape(new));
    let mut out = String::with_capacity(text.len());
    let (mut rest, mut n) = (text, 0);
    while let Some(i) = rest.find(&needle) {
        out.push_str(&rest[..i]);
        let after = &rest[i + needle.len()..];
        if after.starts_with('"') || after.starts_with('/') {
            out.push_str(&rep);
            n += 1;
        } else {
            out.push_str(&needle);
        }
        rest = after;
    }
    out.push_str(rest);
    (out, n)
}

/// Rewrites one file in place via a temp file, keeping permissions and mtime
/// (Claude orders its resume list by mtime). Returns the number of replacements.
pub(crate) fn rewrite_file(path: &Path, key: &str, old: &str, new: &str) -> Result<usize> {
    let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let (out, n) = rewrite_prefix(&text, key, old, new);
    if n == 0 {
        return Ok(0);
    }
    let meta = fs::metadata(path)?;
    let tmp = path.with_extension("tmp-claude-sessions");
    fs::write(&tmp, out)?;
    fs::set_permissions(&tmp, meta.permissions())?;
    fs::File::options().write(true).open(&tmp)?.set_modified(meta.modified()?)?;
    fs::rename(&tmp, path)?;
    Ok(n)
}

/// Renames JSON object keys that are `old` or below it (`~/.claude.json` keeps
/// per-project trust/MCP state under `projects.<abs path>`). Text-level so the
/// rest of the file keeps its exact bytes. Returns the new text and the keys
/// that were renamed (as their new names).
pub fn rewrite_path_keys(text: &str, old: &str, new: &str) -> Result<(String, Vec<String>)> {
    let needle = format!("\"{}", json_escape(old));
    let rep = format!("\"{}", json_escape(new));
    let is_key = |after_quote: &str| after_quote.trim_start().starts_with(':');
    let mut out = String::with_capacity(text.len());
    let (mut rest, mut renamed) = (text, Vec::new());
    while let Some(i) = rest.find(&needle) {
        out.push_str(&rest[..i]);
        let after = &rest[i + needle.len()..];
        // End of the string token: the first unescaped quote.
        let end = after
            .char_indices()
            .scan(false, |esc, (j, c)| {
                let hit = !*esc && c == '"';
                *esc = !*esc && c == '\\';
                Some((j, hit))
            })
            .find_map(|(j, hit)| hit.then_some(j));
        match end {
            Some(j) if (j == 0 || after.starts_with('/')) && is_key(&after[j + 1..]) => {
                let tail = &after[..j];
                if text.contains(&format!("{rep}{tail}\"")) && !(old == new) {
                    let full = format!("{rep}{tail}\"");
                    if let Some(k) = text.find(&full) {
                        if is_key(&text[k + full.len()..]) {
                            bail!(".claude.json already has a project entry for {}", &full[1..full.len() - 1]);
                        }
                    }
                }
                out.push_str(&rep);
                out.push_str(tail);
                out.push('"');
                renamed.push(format!("{new}{}", &json_unescape(tail)));
                rest = &after[j + 1..];
            }
            _ => {
                out.push_str(&needle);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok((out, renamed))
}

fn json_unescape(s: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{s}\"")).unwrap_or_else(|_| s.to_string())
}

/// `~/.claude.json` lives next to the claude dir (or inside it when
/// CLAUDE_CONFIG_DIR is used); returns the first that exists.
pub(crate) fn claude_json_path(claude_dir: &Path) -> Option<PathBuf> {
    let inside = claude_dir.join(".claude.json");
    let beside = claude_dir.parent()?.join(".claude.json");
    [inside, beside].into_iter().find(|p| p.is_file())
}

fn plan_claude_json(claude_dir: &Path, old: &str, new: &str) -> Result<Option<(PathBuf, String, usize)>> {
    let Some(path) = claude_json_path(claude_dir) else { return Ok(None) };
    let text = fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let (out, renamed) = rewrite_path_keys(&text, old, new)?;
    Ok((!renamed.is_empty()).then_some((path, out, renamed.len())))
}

fn apply_claude_json(path: &Path, out: String) -> Result<PathBuf> {
    let meta = fs::metadata(path)?;
    let bak = crate::sidecar::backup(path)?;
    let tmp = path.with_extension("json.tmp-claude-sessions");
    fs::write(&tmp, out)?;
    fs::set_permissions(&tmp, meta.permissions())?;
    fs::rename(&tmp, path)?;
    Ok(bak)
}

/// Quotes a path for pasting into a shell command.
fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@:,".contains(c)) {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub(crate) fn jsonl_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            jsonl_files_recursive(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

fn move_dir(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let ok = Command::new("cp").arg("-a").arg(src).arg(dst).status()?.success();
            if !ok {
                bail!("cp -a failed; {} is left untouched", src.display());
            }
            fs::remove_dir_all(src)?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("cannot move {} -> {}", src.display(), dst.display())),
    }
}

#[derive(Debug, Clone)]
pub struct RunningClaude {
    pub pid: u32,
    pub cwd: PathBuf,
    pub name: Option<String>,
}

fn same_start(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

fn proc_start(pid: u32) -> Option<String> {
    let o = Command::new("ps")
        .env("TZ", "UTC") // Claude records procStart in UTC
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn cwd_of(pid: u32) -> Option<PathBuf> {
    if let Ok(p) = fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(p);
    }
    let o = Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).output().ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(PathBuf::from))
}

/// Live Claude Code processes with their working directory. `sessions/<pid>.json`
/// can be stale (dead process, reused pid), so it only counts when its recorded
/// start time matches the process now holding that pid. `pgrep` + `lsof` catches
/// processes that have no such file.
pub fn running_claudes(claude_dir: &Path) -> Vec<RunningClaude> {
    let mut out: Vec<RunningClaude> = Vec::new();
    for e in fs::read_dir(claude_dir.join("sessions")).into_iter().flatten().flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let (Some(pid), Some(cwd), Some(start)) = (
            v.get("pid").and_then(|x| x.as_u64()),
            v.get("cwd").and_then(|x| x.as_str()),
            v.get("procStart").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        let pid = pid as u32;
        if proc_start(pid).is_some_and(|now| same_start(&now, start)) {
            out.push(RunningClaude {
                pid,
                cwd: PathBuf::from(cwd),
                name: v.get("name").and_then(|x| x.as_str()).map(String::from),
            });
        }
    }
    if let Ok(o) = Command::new("pgrep").args(["-x", "claude"]).output() {
        for pid in String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse::<u32>().ok()) {
            if out.iter().all(|r| r.pid != pid) {
                if let Some(cwd) = cwd_of(pid) {
                    out.push(RunningClaude { pid, cwd, name: None });
                }
            }
        }
    }
    for r in &mut out {
        if let Ok(c) = resolve(&r.cwd) {
            r.cwd = c;
        }
    }
    out
}

/// Processes whose working directory is at or below any of `dirs`.
pub fn conflicts<'a>(running: &'a [RunningClaude], dirs: &[&Path]) -> Vec<&'a RunningClaude> {
    running.iter().filter(|r| dirs.iter().any(|d| r.cwd.starts_with(d))).collect()
}

/// Every project folder at or below `src` with the folder it gets for `dst`; excludes
/// deliberately don't apply here. Refuses if a target folder already exists.
pub(crate) fn plan_steps(claude_dir: &Path, src: &Path, dst: &Path) -> Result<Vec<Step>> {
    let root = projects_root(claude_dir);
    let mut steps = Vec::new();
    let mut taken = HashSet::new();
    for p in list_projects(claude_dir)? {
        let Ok(rel) = p.cwd.strip_prefix(src) else { continue };
        let new_cwd = if rel.as_os_str().is_empty() { dst.to_path_buf() } else { dst.join(rel) };
        let new_dir = root.join(encode_path(&new_cwd));
        if new_dir != p.dir {
            if new_dir.exists() {
                bail!("session folder {} already exists; refusing to overwrite", new_dir.display());
            }
            if !taken.insert(new_dir.clone()) {
                bail!("two session folders would both become {}", new_dir.display());
            }
        }
        steps.push(Step { project: p, new_dir });
    }
    Ok(steps)
}

pub fn run(o: &Opts) -> Result<()> {
    let src = resolve(&o.src)?;
    let dst = resolve(&o.dst)?;
    if src == dst {
        bail!("source and destination are the same");
    }
    if dst.starts_with(&src) {
        bail!("destination is inside the source directory");
    }
    if o.no_move_files {
        if !dst.is_dir() {
            bail!("--no-move-files: {} does not exist", dst.display());
        }
    } else {
        if !src.is_dir() {
            bail!("{} is not a directory", src.display());
        }
        if dst.exists() {
            bail!("{} already exists", dst.display());
        }
    }

    let steps = plan_steps(&o.claude_dir, &src, &dst)?;

    println!(
        "{} {} {} {}",
        style::bold_cyan(if o.no_move_files { "Sessions only:" } else { "Move:" }),
        style::cyan(&src.display().to_string()),
        style::dim("→"),
        style::green(&dst.display().to_string())
    );
    for s in &steps {
        let n = session_count(&s.project.dir);
        println!(
            "  {}  {} {} {}",
            style::yellow(&format!("{n} session(s)")),
            style::cyan(&folder(&s.project.dir)),
            style::dim("→"),
            style::green(&folder(&s.new_dir))
        );
    }
    if steps.is_empty() {
        println!("  {}", style::dim("(no Claude sessions found for this directory)"));
    }
    let (old, new) = (src.to_string_lossy().into_owned(), dst.to_string_lossy().into_owned());
    let cj = plan_claude_json(&o.claude_dir, &old, &new)?;
    if let Some((path, _, n)) = &cj {
        println!("  {}  {}", style::yellow(&format!("{n} entry(ies)")), style::dim(&format!("in {}", path.display())));
    }
    let running = running_claudes(&o.claude_dir);
    let busy = conflicts(&running, &[&src, &dst]);
    if !busy.is_empty() {
        println!("{}", style::bold_yellow("⚠ Claude Code is running inside the affected directories:"));
        for r in &busy {
            println!(
                "  {}  {}  {}",
                style::yellow(&format!("pid {}", r.pid)),
                r.cwd.display(),
                style::dim(r.name.as_deref().unwrap_or(""))
            );
        }
    }
    if o.dry_run {
        if !busy.is_empty() && !o.force {
            println!("{} a real run would stop here; exit those Claude sessions first (or use --force).", style::bold_yellow("Dry run:"));
        } else {
            println!("{} nothing changed.", style::bold_green("Dry run:"));
        }
        return Ok(());
    }
    if !busy.is_empty() {
        if !o.force {
            bail!("refusing to move while Claude Code is running there; exit those sessions first, or pass --force");
        }
        eprintln!("{} --force given; those sessions may keep writing to the old path", style::bold_yellow("warning:"));
    }

    if !o.no_move_files {
        move_dir(&src, &dst)?;
        println!("{}", style::green("✔ Moved directory."));
    }
    let mut rewritten = 0;
    for s in &steps {
        let mut files = Vec::new();
        jsonl_files_recursive(&s.project.dir, &mut files);
        for f in files {
            rewritten += rewrite_file(&f, "cwd", &old, &new)?;
        }
        if s.new_dir != s.project.dir {
            fs::rename(&s.project.dir, &s.new_dir)
                .with_context(|| format!("cannot rename {}", s.project.dir.display()))?;
        }
    }
    let mut backups = Vec::new();
    let history = o.claude_dir.join("history.jsonl");
    if history.is_file() && rewrite_prefix(&fs::read_to_string(&history)?, "project", &old, &new).1 > 0 {
        backups.push(crate::sidecar::backup(&history)?);
        rewrite_file(&history, "project", &old, &new)?;
    }
    if let Some((path, out, _)) = cj {
        backups.push(apply_claude_json(&path, out)?);
    }
    println!("{}", style::green(&format!("✔ Updated {} session folder(s), {} cwd record(s).", steps.len(), rewritten)));
    for b in &backups {
        println!("  {} {}", style::dim("backup:"), style::dim(&b.display().to_string()));
    }
    let flag = if o.no_move_files { " --no-move-files" } else { "" };
    println!(
        "  {} claude-sessions mv {} {}{flag}{}",
        style::dim("undo:"),
        shell_quote(&dst),
        shell_quote(&src),
        if o.no_move_files { style::dim("   (then move the directory back yourself)") } else { String::new() }
    );
    Ok(())
}

fn folder(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn session_count(dir: &Path) -> usize {
    crate::scan::session_files(dir).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc(pid: u32, cwd: &str) -> RunningClaude {
        RunningClaude { pid, cwd: PathBuf::from(cwd), name: None }
    }

    #[test]
    fn conflicts_use_path_prefix_not_string_prefix() {
        let run = vec![rc(1, "/a/b"), rc(2, "/a/b/c"), rc(3, "/a/bc"), rc(4, "/x")];
        let src = PathBuf::from("/a/b");
        let hit: Vec<u32> = conflicts(&run, &[&src]).iter().map(|r| r.pid).collect();
        assert_eq!(hit, vec![1, 2]);
    }

    #[test]
    fn start_time_compare_ignores_spacing() {
        assert!(same_start("Wed Oct  7 11:57:15 2026", "Wed Oct 7 11:57:15 2026"));
        assert!(!same_start("Wed Oct  7 11:57:15 2026", "Wed Oct  7 11:57:16 2026"));
    }

    #[test]
    fn renames_project_keys_only() {
        let t = "{\n  \"projects\": {\n    \"/a/b\": {\"x\": 1},\n    \"/a/b/c\": {},\n    \"/a/bc\": {},\n    \"/z\": {\"p\": \"/a/b\"}\n  }\n}";
        let (out, r) = rewrite_path_keys(t, "/a/b", "/n b").unwrap();
        assert_eq!(r, vec!["/n b", "/n b/c"]);
        assert!(out.contains("\"/n b\": {\"x\": 1}") && out.contains("\"/n b/c\": {}"));
        assert!(out.contains("\"/a/bc\": {}") && out.contains("\"p\": \"/a/b\""));
        serde_json::from_str::<serde_json::Value>(&out).unwrap();
    }

    #[test]
    fn project_key_collision_is_refused() {
        let t = "{\"/a\": {}, \"/b\": {}}";
        assert!(rewrite_path_keys(t, "/a", "/b").is_err());
    }

    #[test]
    fn rewrites_whole_path_prefix_only() {
        let t = r#"{"cwd":"/a/b","x":1}
{"cwd":"/a/b/c"}
{"cwd":"/a/bc"}
{"cwd":"/z"}"#;
        let (out, n) = rewrite_prefix(t, "cwd", "/a/b", "/n b");
        assert_eq!(n, 2);
        assert!(out.contains(r#"{"cwd":"/n b","x":1}"#));
        assert!(out.contains(r#"{"cwd":"/n b/c"}"#));
        assert!(out.contains(r#"{"cwd":"/a/bc"}"#));
        assert!(out.contains(r#"{"cwd":"/z"}"#));
    }
}
