use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::mv::{conflicts, running_claudes};
use crate::scan::{list_projects, resolve, session_files, strip_prefix_ci};
use crate::sidecar::{self, Edit, path_size, remove_path, session_ids, sidecar_paths};
use crate::stats::analyze_session;
use crate::style;
use crate::timespec::{iso, now_secs};

pub struct Opts {
    pub claude_dir: PathBuf,
    /// `Some`: sessions of this directory and below (`rm`); `None`: folders whose
    /// directory no longer exists (`clean`).
    pub target: Option<PathBuf>,
    /// Only sessions last active more than this many seconds ago.
    pub older_than: Option<i64>,
    pub dry_run: bool,
    pub yes: bool,
    pub force: bool,
    /// Also drop the matching `history.jsonl` lines and (for whole folders) the
    /// `~/.claude.json` project entry. Off by default: that is your prompt history.
    pub purge_config: bool,
    /// Ask about every folder instead of once for all.
    pub interactive: bool,
    /// When a whole folder goes, keep its auto-memory (`memory/`).
    pub keep_memory: bool,
}

/// One project folder (or some of its sessions) that is about to go.
pub struct Item {
    pub dir: PathBuf,
    pub cwd: PathBuf,
    /// Delete the whole project folder instead of just `files`.
    pub whole: bool,
    /// Transcripts to delete when not `whole`.
    pub files: Vec<PathBuf>,
    /// Sessions affected; their data outside the project folder goes too.
    pub ids: Vec<String>,
    pub sessions: usize,
    pub bytes: u64,
    /// Files in the project's auto-memory folder, which goes with a whole-folder delete.
    pub memory_files: usize,
    /// Delete everything of a whole folder but its `memory/`.
    pub keep_memory: bool,
}

fn ids_bytes(claude_dir: &Path, ids: &[String]) -> u64 {
    ids.iter().flat_map(|id| sidecar_paths(claude_dir, id)).map(|p| path_size(&p)).sum()
}

/// Every session of a project folder, with the project folder itself.
pub fn project_item(claude_dir: &Path, dir: &Path, cwd: &Path) -> Item {
    let ids = session_ids(dir);
    let bytes = path_size(dir) + ids_bytes(claude_dir, &ids);
    Item { dir: dir.to_path_buf(), cwd: cwd.to_path_buf(), whole: true, files: vec![], ids, sessions: session_files(dir).len(), bytes, memory_files: crate::memory::count(dir), keep_memory: false }
}

/// Just these transcripts of a project folder, with their per-session data.
pub fn sessions_item(claude_dir: &Path, dir: &Path, cwd: &Path, files: Vec<PathBuf>) -> Item {
    let ids: Vec<String> = files.iter().filter_map(|f| f.file_stem()).map(|s| s.to_string_lossy().into_owned()).collect();
    let bytes = files.iter().map(|f| path_size(f) + path_size(&f.with_extension(""))).sum::<u64>() + ids_bytes(claude_dir, &ids);
    Item { dir: dir.to_path_buf(), cwd: cwd.to_path_buf(), whole: false, sessions: files.len(), files, ids, bytes, memory_files: 0, keep_memory: false }
}

pub fn plan(o: &Opts) -> Result<Vec<Item>> {
    let base = o.target.as_deref().map(resolve).transpose()?;
    let cutoff = o.older_than.map(|d| iso(now_secs() - d));
    let mut items = Vec::new();
    for p in list_projects(&o.claude_dir)? {
        match &base {
            Some(b) if strip_prefix_ci(&p.cwd, b).is_none() => continue,
            None if p.cwd.exists() => continue,
            _ => {}
        }
        match &cutoff {
            None => {
                let mut i = project_item(&o.claude_dir, &p.dir, &p.cwd);
                if o.keep_memory && i.memory_files > 0 {
                    i.keep_memory = true;
                    i.bytes -= path_size(&crate::memory::dir_of(&p.dir));
                }
                items.push(i);
            }
            Some(c) => {
                let files: Vec<PathBuf> = session_files(&p.dir)
                    .into_iter()
                    .filter(|f| analyze_session(f).last.is_some_and(|l| l.as_str() < c.as_str()))
                    .collect();
                if !files.is_empty() {
                    items.push(sessions_item(&o.claude_dir, &p.dir, &p.cwd, files));
                }
            }
        }
    }
    Ok(items)
}

/// The `history.jsonl` / `.claude.json` rewrites `--purge-config` would make for `items`.
pub fn config_edits(claude_dir: &Path, items: &[Item]) -> Result<Vec<Edit>> {
    let ids: HashSet<&str> = items.iter().flat_map(|i| i.ids.iter().map(String::as_str)).collect();
    // A folder that keeps its memory keeps its `.claude.json` entry and history too, so
    // the memory can still be told apart and found.
    let whole: HashSet<String> = items.iter().filter(|i| i.whole && !i.keep_memory).map(|i| i.cwd.display().to_string()).collect();
    let mut edits = Vec::new();
    let field = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    edits.extend(sidecar::plan_history(claude_dir, |v| {
        field(v, "sessionId").is_some_and(|s| ids.contains(s.as_str())) || field(v, "project").is_some_and(|p| whole.contains(&p))
    })?);
    match sidecar::plan_claude_json(claude_dir, |k| whole.contains(k)) {
        Ok(e) => edits.extend(e),
        Err(e) => eprintln!("{} {e:#}", style::bold_yellow("warning:")),
    }
    Ok(edits)
}

pub struct Done {
    pub sessions: usize,
    pub folders: usize,
    pub edits: Vec<Edit>,
    /// Backups made of the edited files, in the same order as `edits`.
    pub backups: Vec<PathBuf>,
}

/// Deletes `items` (no questions asked) and, with `purge_config`, applies the config edits.
pub fn execute(o: &Opts, items: &[Item]) -> Result<Done> {
    let edits = if o.purge_config { config_edits(&o.claude_dir, items)? } else { vec![] };
    for i in items {
        if i.whole && i.keep_memory {
            for e in std::fs::read_dir(&i.dir)?.flatten() {
                if e.file_name() != crate::memory::DIR {
                    remove_path(&e.path())?;
                }
            }
        } else if i.whole {
            remove_path(&i.dir)?;
        } else {
            for f in &i.files {
                remove_path(f)?;
                // Per-session data (tool results, subagents) lives in a folder named after the id.
                let side = f.with_extension("");
                if side.is_dir() {
                    remove_path(&side)?;
                }
            }
        }
        for id in &i.ids {
            for p in sidecar_paths(&o.claude_dir, id) {
                remove_path(&p)?;
            }
        }
    }
    let mut backups = Vec::new();
    for e in &edits {
        backups.push(sidecar::apply(e)?);
    }
    Ok(Done { sessions: items.iter().map(|i| i.sessions).sum(), folders: items.len(), edits, backups })
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

fn prompt(question: &str, choices: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!("not a terminal: pass --yes to delete without asking (or --dry-run to preview)");
    }
    print!("{question} {choices} ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_lowercase())
}

pub(crate) fn confirm(question: &str) -> Result<bool> {
    Ok(matches!(prompt(question, "[y/N]")?.as_str(), "y" | "yes"))
}

fn describe_edits(edits: &[Edit]) -> Vec<String> {
    edits
        .iter()
        .map(|e| {
            let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let what = if e.keys.is_empty() { "line(s)" } else { "project entry(ies)" };
            format!("{} {what} in {name}", e.removed)
        })
        .collect()
}

pub fn run(o: &Opts) -> Result<()> {
    let mut items = plan(o)?;
    let what = if o.target.is_some() { "Delete" } else { "Delete orphaned" };
    println!("{}", style::bold_cyan(&format!("{what} sessions")));
    if items.is_empty() {
        println!("  {}", style::dim("(nothing to delete)"));
        return Ok(());
    }
    for i in &items {
        println!(
            "  {}  {}  {}{}",
            style::yellow(&format!("{} session(s)", i.sessions)),
            style::cyan(&i.cwd.display().to_string()),
            style::dim(&mb(i.bytes)),
            match (i.memory_files, i.keep_memory) {
                (0, _) => String::new(),
                (n, true) => style::dim(&format!("  ({n} memory file(s) kept)")),
                (n, false) => style::yellow(&format!("  (+{n} memory file(s))")),
            }
        );
    }

    let running = running_claudes(&o.claude_dir);
    let cwds: Vec<&Path> = items.iter().map(|i| i.cwd.as_path()).collect();
    let busy = conflicts(&running, &cwds);
    if !busy.is_empty() {
        println!("{}", style::bold_yellow("⚠ Claude Code is running in these directories:"));
        for r in &busy {
            println!("  {}  {}", style::yellow(&format!("pid {}", r.pid)), r.cwd.display());
        }
    }
    if o.purge_config {
        for line in describe_edits(&config_edits(&o.claude_dir, &items)?) {
            println!("  {}  {}", style::yellow("config"), style::dim(&line));
        }
    }
    if o.dry_run {
        if !busy.is_empty() && !o.force {
            println!("{} a real run would stop here; exit those Claude sessions first (or use --force).", style::bold_yellow("Dry run:"));
        } else {
            println!("{} nothing deleted.", style::bold_green("Dry run:"));
        }
        return Ok(());
    }
    if !busy.is_empty() && !o.force {
        bail!("refusing to delete while Claude Code is running there; exit those sessions first, or pass --force");
    }

    if o.purge_config && !running.is_empty() && !o.force {
        bail!("--purge-config edits files Claude Code rewrites while it runs ({} process(es) running); exit them first, or pass --force", running.len());
    }

    if o.interactive {
        let (mut chosen, mut all, mut rest) = (Vec::new(), false, items.into_iter());
        for i in rest.by_ref() {
            let take = all || match prompt(&format!("Delete {} ({} session(s), {})?", i.cwd.display(), i.sessions, mb(i.bytes)), "[y/n/a(ll)/q(uit)]")?.as_str() {
                "y" | "yes" => true,
                "a" | "all" => {
                    all = true;
                    true
                }
                "q" | "quit" => break,
                _ => false,
            };
            if take {
                chosen.push(i);
            }
        }
        items = chosen;
        if items.is_empty() {
            println!("{}", style::dim("Nothing selected; nothing deleted."));
            return Ok(());
        }
    } else if !o.yes {
        let (n, b) = (items.iter().map(|i| i.sessions).sum::<usize>(), items.iter().map(|i| i.bytes).sum::<u64>());
        if !confirm(&format!("Permanently delete {n} session(s) ({})?", mb(b)))? {
            println!("{}", style::dim("Aborted; nothing deleted."));
            return Ok(());
        }
    }

    let done = execute(o, &items)?;
    println!("{}", style::green(&format!("✔ Deleted {} session(s) in {} folder(s).", done.sessions, done.folders)));
    if done.edits.is_empty() {
        println!("{}", style::dim("history.jsonl and .claude.json are left alone; `claude-sessions doctor` lists what is now stale."));
    } else {
        for (line, bak) in describe_edits(&done.edits).iter().zip(&done.backups) {
            println!("{}", style::green(&format!("✔ Removed {line}")));
            println!("  {} {}", style::dim("backup:"), style::dim(&bak.display().to_string()));
        }
        println!("{}", style::dim("The sessions themselves are gone for good; a backup only records which history lines and project entries were removed."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn setup(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let claude = tmp.join(".claude");
        let alive = tmp.join("alive");
        fs::create_dir_all(&alive).unwrap();
        let gone = tmp.join("gone");
        for (dir, cwd) in [("a", &alive), ("b", &gone)] {
            let p = claude.join("projects").join(dir);
            fs::create_dir_all(&p).unwrap();
            let c = serde_json::to_string(&cwd.display().to_string()).unwrap();
            fs::write(p.join("old.jsonl"), format!("{{\"cwd\":{c},\"timestamp\":\"2020-01-01T00:00:00Z\"}}\n")).unwrap();
            fs::write(p.join("new.jsonl"), format!("{{\"cwd\":{c},\"timestamp\":\"2999-01-01T00:00:00Z\"}}\n")).unwrap();
        }
        (claude, alive, gone)
    }

    fn opts(claude: PathBuf, target: Option<PathBuf>, older: Option<i64>, dry_run: bool) -> Opts {
        Opts { claude_dir: claude, target, older_than: older, dry_run, yes: true, force: true, purge_config: false, interactive: false, keep_memory: false }
    }

    #[test]
    fn dry_run_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup(&tmp.path().canonicalize().unwrap());
        run(&opts(claude.clone(), Some(alive), None, true)).unwrap();
        assert!(claude.join("projects/a/old.jsonl").exists());
    }

    #[test]
    fn keep_memory_leaves_the_memory_folder_and_its_config() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, proj, _) = setup(&tmp.path().canonicalize().unwrap());
        let p = list_projects(&claude).unwrap().into_iter().find(|p| p.cwd == proj).unwrap();
        fs::create_dir_all(p.dir.join("memory")).unwrap();
        fs::write(p.dir.join("memory/MEMORY.md"), "- [a](a.md)\n").unwrap();
        fs::write(claude.join(".claude.json"), format!("{{\"projects\":{{{}:{{}}}}}}", q(&proj))).unwrap();
        let mut o = opts(claude.clone(), Some(proj.clone()), None, false);
        o.keep_memory = true;
        let items = plan(&o).unwrap();
        assert!(items.iter().any(|i| i.keep_memory));
        assert!(config_edits(&claude, &items).unwrap().is_empty());
        execute(&o, &items).unwrap();
        assert!(p.dir.join("memory/MEMORY.md").is_file());
        assert!(session_files(&p.dir).is_empty());
    }

    #[test]
    fn rm_older_than_keeps_recent_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup(&tmp.path().canonicalize().unwrap());
        fs::create_dir_all(claude.join("projects/a/old")).unwrap();
        run(&opts(claude.clone(), Some(alive), Some(86_400), false)).unwrap();
        assert!(!claude.join("projects/a/old.jsonl").exists());
        assert!(!claude.join("projects/a/old").exists());
        assert!(claude.join("projects/a/new.jsonl").exists());
        assert!(claude.join("projects/b/old.jsonl").exists());
    }

    #[test]
    fn rm_without_age_removes_whole_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup(&tmp.path().canonicalize().unwrap());
        run(&opts(claude.clone(), Some(alive), None, false)).unwrap();
        assert!(!claude.join("projects/a").exists());
        assert!(claude.join("projects/b").exists());
    }

    #[test]
    fn clean_only_touches_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, _, _) = setup(&tmp.path().canonicalize().unwrap());
        run(&opts(claude.clone(), None, None, false)).unwrap();
        assert!(claude.join("projects/a").exists());
        assert!(!claude.join("projects/b").exists());
    }

    fn q(p: &Path) -> String {
        serde_json::to_string(&p.display().to_string()).unwrap()
    }

    /// `setup` plus per-session side data, history lines and a .claude.json.
    fn setup_full(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let (claude, alive, gone) = setup(tmp);
        for (proj, id) in [("a", "old"), ("a", "new"), ("b", "old"), ("b", "new")] {
            fs::create_dir_all(claude.join("projects").join(proj).join(id).join("subagents")).unwrap();
        }
        for id in ["old", "new"] {
            fs::create_dir_all(claude.join("file-history").join(id)).unwrap();
            fs::write(claude.join("file-history").join(id).join("x@v2"), "data").unwrap();
            fs::create_dir_all(claude.join("session-env").join(id)).unwrap();
        }
        fs::create_dir_all(claude.join("plans")).unwrap();
        fs::write(claude.join("plans/p.md"), "plan").unwrap();
        let hist = format!(
            "{{\"sessionId\":\"old\",\"project\":{a}}}\n{{\"sessionId\":\"zzz\",\"project\":{a}}}\n{{\"sessionId\":\"keep\",\"project\":{g}}}\n",
            a = q(&alive),
            g = q(&gone)
        );
        fs::write(claude.join("history.jsonl"), hist).unwrap();
        fs::write(claude.join(".claude.json"), format!("{{\"projects\":{{{}:{{}},{}:{{}}}}}}", q(&alive), q(&gone))).unwrap();
        (claude, alive, gone)
    }

    #[test]
    fn rm_older_than_removes_that_sessions_side_data_only() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup_full(&tmp.path().canonicalize().unwrap());
        run(&opts(claude.clone(), Some(alive), Some(86_400), false)).unwrap();
        assert!(!claude.join("projects/a/old").exists());
        assert!(!claude.join("file-history/old").exists());
        assert!(!claude.join("session-env/old").exists());
        assert!(claude.join("file-history/new").exists());
        assert!(claude.join("plans/p.md").exists());
        assert_eq!(fs::read_to_string(claude.join("history.jsonl")).unwrap().lines().count(), 3, "config untouched by default");
    }

    #[test]
    fn whole_folder_removal_takes_side_data_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup_full(&tmp.path().canonicalize().unwrap());
        run(&opts(claude.clone(), Some(alive), None, false)).unwrap();
        assert!(!claude.join("projects/a").exists());
        assert!(!claude.join("file-history/old").exists() && !claude.join("file-history/new").exists());
        assert!(claude.join("plans/p.md").exists());
    }

    #[test]
    fn purge_config_edits_history_and_claude_json_with_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, gone) = setup_full(&tmp.path().canonicalize().unwrap());
        let mut o = opts(claude.clone(), Some(alive.clone()), None, false);
        o.purge_config = true;
        run(&o).unwrap();
        let h = fs::read_to_string(claude.join("history.jsonl")).unwrap();
        assert_eq!(h.lines().count(), 1, "session ids and the whole project's lines are dropped: {h}");
        assert!(h.contains("keep"));
        let cj: serde_json::Value = serde_json::from_str(&fs::read_to_string(claude.join(".claude.json")).unwrap()).unwrap();
        let keys: Vec<&String> = cj["projects"].as_object().unwrap().keys().collect();
        assert_eq!(keys, vec![&gone.display().to_string()]);
        assert_eq!(sidecar::backups_of(&claude.join("history.jsonl")).len(), 1);
        assert_eq!(sidecar::backups_of(&claude.join(".claude.json")).len(), 1);
        assert!(!claude.join("history.jsonl.bak").exists());
    }

    #[test]
    fn purge_config_keeps_project_entry_when_only_old_sessions_go() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup_full(&tmp.path().canonicalize().unwrap());
        let mut o = opts(claude.clone(), Some(alive.clone()), Some(86_400), false);
        o.purge_config = true;
        run(&o).unwrap();
        let cj = fs::read_to_string(claude.join(".claude.json")).unwrap();
        let cj: serde_json::Value = serde_json::from_str(&cj).unwrap();
        assert!(cj["projects"].as_object().unwrap().contains_key(&alive.display().to_string()));
        let h = fs::read_to_string(claude.join("history.jsonl")).unwrap();
        assert!(!h.contains("\"old\"") && h.contains("zzz"));
    }

    #[test]
    fn purge_config_dry_run_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, alive, _) = setup_full(&tmp.path().canonicalize().unwrap());
        let before = fs::read_to_string(claude.join("history.jsonl")).unwrap();
        let mut o = opts(claude.clone(), Some(alive), None, true);
        o.purge_config = true;
        run(&o).unwrap();
        assert_eq!(fs::read_to_string(claude.join("history.jsonl")).unwrap(), before);
        assert!(sidecar::backups_of(&claude.join("history.jsonl")).is_empty());
    }
}
