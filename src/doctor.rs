use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::Serialize;

use crate::mv::{claude_json_path, running_claudes};
use crate::rm;
use crate::sidecar::{self, Edit, count_files, looks_like_session_id, path_size, remove_path};
use crate::scan::{list_projects, projects_root, session_files};
use crate::style;

/// A session folder whose working directory no longer exists.
#[derive(Serialize)]
pub struct Orphan {
    pub folder: String,
    pub cwd: String,
    pub sessions: usize,
    pub bytes: u64,
}

/// A path recorded in `history.jsonl` or `.claude.json` that no longer exists.
#[derive(Serialize)]
pub struct StalePath {
    pub path: String,
    pub entries: usize,
}

/// Per-session data (`file-history/<id>`, `session-env/<id>`, ...) whose session has no transcript.
#[derive(Serialize)]
pub struct Dangling {
    pub path: PathBuf,
    pub bytes: u64,
}

/// A `projects/` folder with no transcript, so no working directory can be worked out
/// and `list_projects` never shows it.
#[derive(Serialize)]
pub struct EmptyProject {
    pub path: PathBuf,
    /// Files in its `memory/` folder. With any, it is the user's auto-memory and is never deleted here.
    pub memory_files: usize,
}

impl EmptyProject {
    pub fn deletable(&self) -> bool {
        self.memory_files == 0
    }
}

#[derive(Serialize, Default)]
pub struct Diagnosis {
    pub orphan_projects: Vec<Orphan>,
    pub stale_history: Vec<StalePath>,
    pub stale_claude_json: Vec<StalePath>,
    pub dangling: Vec<Dangling>,
    pub empty_projects: Vec<EmptyProject>,
}

impl Diagnosis {
    /// Folders that only hold auto-memory are reported but never count as a problem.
    pub fn is_clean(&self) -> bool {
        self.orphan_projects.is_empty()
            && self.stale_history.is_empty()
            && self.stale_claude_json.is_empty()
            && self.dangling.is_empty()
            && self.empty_projects.iter().all(|p| !p.deletable())
    }
}

/// Anything touched more recently than this may belong to a session that is starting up
/// (Claude can create its folders before the transcript exists), so it is left alone.
const GRACE: Duration = Duration::from_secs(24 * 3600);

fn recent(p: &Path) -> bool {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age < GRACE)
}

fn find_dangling(claude_dir: &Path) -> Vec<Dangling> {
    let known = sidecar::all_session_ids(claude_dir);
    let live = sidecar::running_session_ids(claude_dir);
    let mut out = Vec::new();
    for dir in ["file-history", "session-env", "tasks", "debug"] {
        for e in fs::read_dir(claude_dir.join(dir)).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".txt").unwrap_or(&name);
            // Only entries shaped like a session id are ever considered per-session data.
            if !looks_like_session_id(id) || known.contains(id) || live.contains(id) || recent(&e.path()) {
                continue;
            }
            out.push(Dangling { bytes: path_size(&e.path()), path: e.path() });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn find_empty_projects(claude_dir: &Path) -> Vec<EmptyProject> {
    let mut out = Vec::new();
    for e in fs::read_dir(projects_root(claude_dir)).into_iter().flatten().flatten() {
        let dir = e.path();
        if !dir.is_dir() || !session_files(&dir).is_empty() || recent(&dir) {
            continue;
        }
        // Anything besides `memory/` (session folders, stray files) means it isn't a plain leftover.
        let other = fs::read_dir(&dir).into_iter().flatten().flatten().any(|c| c.file_name() != "memory");
        if other {
            continue;
        }
        out.push(EmptyProject { memory_files: count_files(&dir.join("memory")), path: dir });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Read-only: nothing is modified.
pub fn diagnose(claude_dir: &Path) -> Result<Diagnosis> {
    let mut d = Diagnosis::default();
    if projects_root(claude_dir).is_dir() {
        for p in list_projects(claude_dir)? {
            if p.cwd.exists() {
                continue;
            }
            let files = session_files(&p.dir);
            d.orphan_projects.push(Orphan {
                folder: p.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                cwd: p.cwd.display().to_string(),
                sessions: files.len(),
                bytes: files.iter().filter_map(|f| fs::metadata(f).ok()).map(|m| m.len()).sum(),
            });
        }
    }

    // history.jsonl: one line per prompt, with the project it was typed in.
    if let Ok(text) = fs::read_to_string(claude_dir.join("history.jsonl")) {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if let Some(p) = v.get("project").and_then(|p| p.as_str()).filter(|p| !p.is_empty()) {
                *counts.entry(p.to_string()).or_default() += 1;
            }
        }
        d.stale_history = counts
            .into_iter()
            .filter(|(p, _)| !Path::new(p).exists())
            .map(|(path, entries)| StalePath { path, entries })
            .collect();
    }

    // .claude.json: per-project trust / MCP state under `projects.<abs path>`.
    if let Some(path) = claude_json_path(claude_dir) {
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(projects) = v.get("projects").and_then(|p| p.as_object()) {
                    d.stale_claude_json = projects
                        .keys()
                        .filter(|k| !Path::new(k).exists())
                        .map(|k| StalePath { path: k.clone(), entries: 1 })
                        .collect();
                }
            }
        }
    }
    d.dangling = find_dangling(claude_dir);
    d.empty_projects = find_empty_projects(claude_dir);
    Ok(d)
}

/// The edits `doctor --fix` would make: `history.jsonl` lines and `.claude.json` project
/// entries for directories that no longer exist. Files on disk are only touched with `--delete`.
pub fn fix_edits(claude_dir: &Path) -> Result<Vec<Edit>> {
    let mut edits = Vec::new();
    let missing = |p: &str| !p.is_empty() && !Path::new(p).exists();
    edits.extend(sidecar::plan_history(claude_dir, |v| v.get("project").and_then(|p| p.as_str()).is_some_and(missing))?);
    edits.extend(sidecar::plan_claude_json(claude_dir, missing)?);
    Ok(edits)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `doctor --fix`: backs each file up (see `sidecar::backup`), then rewrites it without the
/// stale entries; that can be undone completely. With `delete` it also removes, for good,
/// the orphaned session folders, per-session data without a transcript and empty project
/// folders. Folders holding auto-memory are never deleted.
pub fn fix(claude_dir: &Path, dry_run: bool, yes: bool, force: bool, delete: bool) -> Result<()> {
    let edits = fix_edits(claude_dir)?;
    let diag = diagnose(claude_dir)?;
    let items = if delete {
        rm::plan(&rm::Opts {
            claude_dir: claude_dir.to_path_buf(),
            target: None,
            older_than: None,
            dry_run: true,
            yes: true,
            force: true,
            purge_config: false,
            interactive: false,
        })?
    } else {
        vec![]
    };
    let empties: Vec<&EmptyProject> = diag.empty_projects.iter().filter(|p| p.deletable()).collect();

    println!("{}", style::bold_cyan("Records (backed up first; can be undone)"));
    if edits.is_empty() {
        println!("  {}", style::dim("(none)"));
    }
    for e in &edits {
        let what = if e.keys.is_empty() { "line(s)" } else { "project entry(ies)" };
        println!("  {}  {}", style::yellow(&format!("{} {what}", e.removed)), style::dim(&format!("in {}", name_of(&e.path))));
        for k in &e.keys {
            println!("      {}", style::cyan(k));
        }
    }

    let nothing_to_delete = items.is_empty() && diag.dangling.is_empty() && empties.is_empty();
    if delete {
        println!("{}", style::bold_red("Files (deleted for good; cannot be undone)"));
        if nothing_to_delete {
            println!("  {}", style::dim("(none)"));
        }
        for i in &items {
            println!(
                "  {}  {}  {}{}",
                style::yellow(&format!("{} session(s)", i.sessions)),
                style::cyan(&i.cwd.display().to_string()),
                style::dim(&mb(i.bytes)),
                if i.memory_files > 0 { style::yellow(&format!("  (+{} memory file(s))", i.memory_files)) } else { String::new() }
            );
        }
        for d in &diag.dangling {
            println!("  {}  {}  {}", style::yellow("no transcript"), style::cyan(&d.path.display().to_string()), style::dim(&mb(d.bytes)));
        }
        for p in &empties {
            println!("  {}  {}", style::yellow("empty folder"), style::cyan(&p.path.display().to_string()));
        }
    } else {
        let left = [
            (diag.orphan_projects.len(), "session folder(s) of missing directories"),
            (diag.dangling.len(), "per-session data item(s) without a transcript"),
            (empties.len(), "empty project folder(s)"),
        ];
        if left.iter().any(|(n, _)| *n > 0) {
            println!("{}", style::bold_cyan("Left in place (nothing on disk is deleted without --delete)"));
            for (n, what) in left.iter().filter(|(n, _)| *n > 0) {
                println!("  {}  {}", style::yellow(&n.to_string()), what);
            }
            println!("  {} {}", style::dim("→"), style::bold("claude-sessions doctor --fix --delete"));
        }
    }
    for p in diag.empty_projects.iter().filter(|p| !p.deletable()) {
        println!("  {} {} holds {} memory file(s); never deleted here (use `claude purge`)", style::bold_cyan("note:"), style::cyan(&p.path.display().to_string()), p.memory_files);
    }

    if edits.is_empty() && (!delete || nothing_to_delete) {
        println!("{}", style::dim("Nothing to fix."));
        return Ok(());
    }
    if dry_run {
        println!("{} nothing changed.", style::bold_green("Dry run:"));
        return Ok(());
    }
    let running = running_claudes(claude_dir);
    if !edits.is_empty() && !running.is_empty() && !force {
        anyhow::bail!("Claude Code is running ({} process(es)) and rewrites these files itself; exit it first, or pass --force", running.len());
    }
    if !yes {
        let q = if delete && !nothing_to_delete {
            let n: usize = items.iter().map(|i| i.sessions).sum();
            format!("Fix the records (backed up) and PERMANENTLY delete {n} session(s) and {} other item(s)? This cannot be undone.", diag.dangling.len() + empties.len() + items.len())
        } else {
            "Remove these records? Each file is backed up first.".to_string()
        };
        if !crate::rm::confirm(&q)? {
            println!("{}", style::dim("Aborted; nothing changed."));
            return Ok(());
        }
    }

    for e in &edits {
        let bak = sidecar::apply(e)?;
        println!("  {} {}", style::dim("backup:"), style::dim(&bak.display().to_string()));
    }
    if !edits.is_empty() {
        println!("{}", style::green("✔ Records fixed. To undo, copy a backup over the original file."));
    }
    if delete {
        let opts = rm::Opts { claude_dir: claude_dir.to_path_buf(), target: None, older_than: None, dry_run: false, yes: true, force, purge_config: false, interactive: false };
        let done = rm::execute(&opts, &items)?;
        for d in &diag.dangling {
            remove_path(&d.path)?;
        }
        for p in &empties {
            remove_path(&p.path)?;
        }
        println!(
            "{}",
            style::green(&format!(
                "✔ Deleted {} session(s) in {} folder(s), {} per-session item(s), {} empty folder(s).",
                done.sessions,
                done.folders,
                diag.dangling.len(),
                empties.len()
            ))
        );
    }
    Ok(())
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

pub fn print_text(d: &Diagnosis) {
    let memory_only: Vec<&EmptyProject> = d.empty_projects.iter().filter(|p| !p.deletable()).collect();
    if d.is_clean() {
        println!("{}", style::bold_green("✔ Nothing to fix."));
        for p in memory_only {
            println!("  {} {} holds only {} memory file(s) and no sessions", style::bold_cyan("note:"), style::cyan(&p.path.display().to_string()), p.memory_files);
        }
        return;
    }
    if !d.orphan_projects.is_empty() {
        println!("{}", style::bold_cyan("Session folders whose directory no longer exists"));
        for o in &d.orphan_projects {
            println!(
                "  {}  {}  {}",
                style::yellow(&format!("{} session(s), {}", o.sessions, mb(o.bytes))),
                style::cyan(&o.cwd),
                style::dim(&format!("({})", o.folder))
            );
        }
        println!(
            "  {} moved by hand? {}   gone for good? {}\n",
            style::dim("→"),
            style::bold("claude-sessions mv <old> <new> --no-move-files"),
            style::bold("claude-sessions doctor --fix --delete")
        );
    }
    if !d.stale_history.is_empty() {
        println!("{}", style::bold_cyan("history.jsonl entries for missing directories"));
        for s in &d.stale_history {
            println!("  {}  {}", style::yellow(&format!("{} entry(ies)", s.entries)), style::cyan(&s.path));
        }
        println!();
    }
    if !d.stale_claude_json.is_empty() {
        println!("{}", style::bold_cyan(".claude.json project entries for missing directories"));
        for s in &d.stale_claude_json {
            println!("  {}", style::cyan(&s.path));
        }
        println!();
    }
    if !d.dangling.is_empty() {
        println!("{}", style::bold_cyan("Per-session data whose session has no transcript (file-history, session-env, ...)"));
        for x in &d.dangling {
            println!("  {}  {}", style::yellow(&mb(x.bytes)), style::cyan(&x.path.display().to_string()));
        }
        println!();
    }
    let deletable: Vec<&EmptyProject> = d.empty_projects.iter().filter(|p| p.deletable()).collect();
    if !deletable.is_empty() {
        println!("{}", style::bold_cyan("Empty project folders (no transcript, so no directory can be told; hidden from other commands)"));
        for p in deletable {
            println!("  {}", style::cyan(&p.path.display().to_string()));
        }
        println!();
    }
    for p in memory_only {
        println!("{} {} holds only {} memory file(s) and no sessions; never deleted by this tool", style::bold_cyan("note:"), style::cyan(&p.path.display().to_string()), p.memory_files);
    }
    println!("{} {}", style::dim("Fix the records (undoable):"), style::bold("claude-sessions doctor --fix"));
    println!("{} {}", style::dim("Also delete the files above for good:"), style::bold("claude-sessions doctor --fix --delete"));
    println!(
        "{}",
        style::dim("A path counts as missing when it doesn't exist now; an unmounted drive looks the same. Anything touched in the last 24 hours is skipped.")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_orphans_and_stale_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".claude");
        let alive = tmp.path().join("alive");
        fs::create_dir_all(&alive).unwrap();
        let gone = tmp.path().join("gone");
        for (dir, cwd) in [("a", &alive), ("b", &gone)] {
            let p = claude.join("projects").join(dir);
            fs::create_dir_all(&p).unwrap();
            let line = format!("{{\"cwd\":{}}}\n", serde_json::to_string(&cwd.display().to_string()).unwrap());
            fs::write(p.join("s.jsonl"), line).unwrap();
        }
        let hist = format!(
            "{{\"project\":{}}}\n{{\"project\":{}}}\n{{\"project\":{}}}\n",
            serde_json::to_string(&alive.display().to_string()).unwrap(),
            serde_json::to_string(&gone.display().to_string()).unwrap(),
            serde_json::to_string(&gone.display().to_string()).unwrap()
        );
        fs::write(claude.join("history.jsonl"), hist).unwrap();
        let cj = format!(
            "{{\"projects\":{{{}:{{}},{}:{{}}}}}}",
            serde_json::to_string(&alive.display().to_string()).unwrap(),
            serde_json::to_string(&gone.display().to_string()).unwrap()
        );
        fs::write(claude.join(".claude.json"), cj).unwrap();

        let d = diagnose(&claude).unwrap();
        assert_eq!(d.orphan_projects.len(), 1);
        assert_eq!(d.orphan_projects[0].cwd, gone.display().to_string());
        assert_eq!(d.orphan_projects[0].sessions, 1);
        assert_eq!(d.stale_history.len(), 1);
        assert_eq!(d.stale_history[0].entries, 2);
        assert_eq!(d.stale_claude_json.len(), 1);
        assert!(!d.is_clean());
    }

    #[test]
    fn fix_removes_only_stale_records() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let (alive, gone) = (tmp.path().join("alive"), tmp.path().join("gone"));
        fs::create_dir_all(&alive).unwrap();
        let q = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
        let hist = format!("{{\"project\":{}}}\n{{\"project\":{}}}\n", q(&alive), q(&gone));
        fs::write(claude.join("history.jsonl"), &hist).unwrap();
        fs::write(claude.join(".claude.json"), format!("{{\"projects\":{{{}:{{}},{}:{{}}}}}}", q(&alive), q(&gone))).unwrap();

        fix(&claude, true, true, true, false).unwrap();
        assert_eq!(fs::read_to_string(claude.join("history.jsonl")).unwrap(), hist, "dry run changes nothing");

        fix(&claude, false, true, true, false).unwrap();
        let h = fs::read_to_string(claude.join("history.jsonl")).unwrap();
        assert!(h.contains("alive") && !h.contains("gone"));
        let cj = fs::read_to_string(claude.join(".claude.json")).unwrap();
        assert!(cj.contains("alive") && !cj.contains("gone"));
        assert_eq!(sidecar::backups_of(&claude.join("history.jsonl")).len(), 1);
        assert_eq!(sidecar::backups_of(&claude.join(".claude.json")).len(), 1);
        // The backup is a full undo: restoring it brings back exactly the original.
        let bak = &sidecar::backups_of(&claude.join("history.jsonl"))[0];
        assert_eq!(fs::read_to_string(bak).unwrap(), hist);
        assert!(fix_edits(&claude).unwrap().is_empty());
    }

    const OLD_ID: &str = "11111111-1111-1111-1111-111111111111";
    const KEPT_ID: &str = "22222222-2222-2222-2222-222222222222";
    const NEW_ID: &str = "33333333-3333-3333-3333-333333333333";
    const LIVE_ID: &str = "44444444-4444-4444-4444-444444444444";

    fn age(p: &Path, days: u64) {
        let t = SystemTime::now() - Duration::from_secs(days * 86_400);
        fs::File::open(p).unwrap().set_modified(t).unwrap();
    }

    /// An orphan project, a live project, per-session data (old / kept / brand new / live),
    /// an empty project folder, a memory-only folder and things that must never be touched.
    fn world(root: &Path) -> (PathBuf, PathBuf) {
        let claude = root.join(".claude");
        let alive = root.join("alive");
        fs::create_dir_all(&alive).unwrap();
        let gone = root.join("gone");
        let q = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
        for (dir, cwd, id) in [("alive", &alive, KEPT_ID), ("gone", &gone, "55555555-5555-5555-5555-555555555555")] {
            let p = claude.join("projects").join(dir);
            fs::create_dir_all(p.join("memory")).unwrap();
            fs::write(p.join(format!("{id}.jsonl")), format!("{{\"cwd\":{}}}\n", q(cwd))).unwrap();
        }
        fs::write(claude.join("projects/gone/memory/m.md"), "mem").unwrap();
        for (dir, id) in [("file-history", OLD_ID), ("file-history", KEPT_ID), ("file-history", NEW_ID), ("session-env", LIVE_ID), ("file-history", "55555555-5555-5555-5555-555555555555")] {
            let p = claude.join(dir).join(id);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("x@v2"), "data").unwrap();
            if id != NEW_ID {
                age(&p, 3);
            }
        }
        fs::create_dir_all(claude.join("tasks/plans-not-a-session")).unwrap();
        age(&claude.join("tasks/plans-not-a-session"), 3);
        fs::create_dir_all(claude.join("sessions")).unwrap();
        fs::write(claude.join("sessions/1.json"), format!("{{\"pid\":1,\"sessionId\":\"{LIVE_ID}\"}}")).unwrap();
        fs::create_dir_all(claude.join("projects/empty/memory")).unwrap();
        age(&claude.join("projects/empty"), 3);
        fs::create_dir_all(claude.join("projects/memonly/memory")).unwrap();
        fs::write(claude.join("projects/memonly/memory/m.md"), "mine").unwrap();
        age(&claude.join("projects/memonly"), 3);
        fs::create_dir_all(claude.join("plans")).unwrap();
        fs::write(claude.join("plans/p.md"), "plan").unwrap();
        fs::write(claude.join("history.jsonl"), format!("{{\"project\":{}}}\n{{\"project\":{}}}\n", q(&alive), q(&gone))).unwrap();
        (claude, gone)
    }

    #[test]
    fn finds_hidden_leftovers_but_skips_recent_live_and_unrelated() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, _) = world(&tmp.path().canonicalize().unwrap());
        let d = diagnose(&claude).unwrap();
        let dangling: Vec<String> = d.dangling.iter().map(|x| x.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(dangling, vec![OLD_ID], "only the old one without a transcript: {dangling:?}");
        let empties: Vec<(String, usize)> = d.empty_projects.iter().map(|p| (p.path.file_name().unwrap().to_string_lossy().into_owned(), p.memory_files)).collect();
        assert_eq!(empties, vec![("empty".to_string(), 0), ("memonly".to_string(), 1)]);
        assert!(!d.is_clean());
    }

    #[test]
    fn memory_only_folders_are_never_a_problem_by_themselves() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".claude");
        fs::create_dir_all(claude.join("projects/memonly/memory")).unwrap();
        fs::write(claude.join("projects/memonly/memory/m.md"), "x").unwrap();
        age(&claude.join("projects/memonly"), 3);
        let d = diagnose(&claude).unwrap();
        assert_eq!(d.empty_projects.len(), 1);
        assert!(d.is_clean());
    }

    #[test]
    fn fix_without_delete_leaves_every_file_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, _) = world(&tmp.path().canonicalize().unwrap());
        fix(&claude, false, true, true, false).unwrap();
        for p in ["projects/gone", "projects/empty", "projects/memonly", "file-history/11111111-1111-1111-1111-111111111111"] {
            assert!(claude.join(p).exists(), "{p} must survive --fix");
        }
        let h = fs::read_to_string(claude.join("history.jsonl")).unwrap();
        assert_eq!(h.lines().count(), 1, "only the stale history line went");
    }

    #[test]
    fn fix_delete_removes_files_for_good_and_spares_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, _) = world(&tmp.path().canonicalize().unwrap());
        fix(&claude, true, true, true, true).unwrap();
        assert!(claude.join("projects/gone").exists(), "dry run deletes nothing");

        fix(&claude, false, true, true, true).unwrap();
        for gone in ["projects/gone", "projects/empty", "file-history/11111111-1111-1111-1111-111111111111", "file-history/55555555-5555-5555-5555-555555555555"] {
            assert!(!claude.join(gone).exists(), "{gone} should be deleted");
        }
        for kept in [
            "projects/alive",
            "projects/memonly/memory/m.md",
            "plans/p.md",
            "tasks/plans-not-a-session",
            "file-history/22222222-2222-2222-2222-222222222222",
            "file-history/33333333-3333-3333-3333-333333333333",
            "session-env/44444444-4444-4444-4444-444444444444",
        ] {
            assert!(claude.join(kept).exists(), "{kept} must be kept");
        }
        assert!(diagnose(&claude).unwrap().is_clean(), "nothing left to report afterwards");
        assert_eq!(sidecar::backups_of(&claude.join("history.jsonl")).len(), 1);
    }

    #[test]
    fn missing_claude_dir_is_clean() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(diagnose(&tmp.path().join("nope")).unwrap().is_clean());
    }
}
