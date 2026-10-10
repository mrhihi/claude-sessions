//! Claude Code's auto-memory: `projects/<encoded>/memory/` holds a `MEMORY.md` index
//! (one `- [Title](file.md) — hook` line per memory) and one Markdown file per memory.
//! Claude keys the folder by git repository root, so subdirectories and worktrees of a
//! repository share it.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::encode::encode_path;
use crate::mv::{conflicts, running_claudes};
use crate::scan::{Project, list_all_projects, projects_root, resolve};
use crate::sidecar::{backup, count_files};
use crate::style;
use crate::timespec::iso;

pub const DIR: &str = "memory";
pub const INDEX: &str = "MEMORY.md";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct MemoryFile {
    /// File name inside `memory/`.
    pub file: String,
    #[serde(skip)]
    pub path: PathBuf,
    /// `name:` from the frontmatter.
    pub name: Option<String>,
    pub description: Option<String>,
    /// `type:` from the frontmatter (`user`, `feedback`, `project`, `reference`).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub bytes: u64,
    pub modified: Option<String>,
}

impl MemoryFile {
    pub fn is_index(&self) -> bool {
        self.file == INDEX
    }

    /// Short label for listings: the description, else the frontmatter name.
    pub fn summary(&self) -> String {
        self.description.clone().or_else(|| self.name.clone()).unwrap_or_default()
    }
}

pub fn dir_of(project_dir: &Path) -> PathBuf {
    project_dir.join(DIR)
}

/// Files in the project's memory folder (0 when there is none).
pub fn count(project_dir: &Path) -> usize {
    count_files(&dir_of(project_dir))
}

/// `name`, `description` and `type` from a `---` frontmatter block at the top of `text`.
fn frontmatter(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut lines = text.lines();
    let (mut name, mut desc, mut kind) = (None, None, None);
    if lines.next().map(str::trim) != Some("---") {
        return (name, desc, kind);
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        if v.is_empty() {
            continue;
        }
        match k.trim() {
            "name" => name = Some(v),
            "description" => desc = Some(v),
            "type" => kind = Some(v),
            _ => {}
        }
    }
    (name, desc, kind)
}

fn load(path: &Path) -> Option<MemoryFile> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let file = path.file_name()?.to_string_lossy().into_owned();
    let (name, description, kind) = frontmatter(&fs::read_to_string(path).unwrap_or_default());
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| iso(d.as_secs() as i64));
    Some(MemoryFile { file, path: path.to_path_buf(), name, description, kind, bytes: meta.len(), modified })
}

/// The memory files of a project folder, `MEMORY.md` first, the rest by name.
pub fn list(project_dir: &Path) -> Vec<MemoryFile> {
    let mut v: Vec<MemoryFile> = fs::read_dir(dir_of(project_dir)).into_iter().flatten().flatten().filter_map(|e| load(&e.path())).collect();
    v.sort_by(|a, b| (!a.is_index(), &a.file).cmp(&(!b.is_index(), &b.file)));
    v
}

/// The memory file `query` names: its file name (with or without `.md`), its frontmatter
/// name, or a unique prefix of either.
pub fn find(project_dir: &Path, query: &str) -> Result<MemoryFile> {
    if query.is_empty() || query.contains(['/', '\\']) || query.contains("..") {
        bail!("'{query}' is not a memory file name");
    }
    let files = list(project_dir);
    let stem = |f: &MemoryFile| f.file.strip_suffix(".md").unwrap_or(&f.file).to_string();
    if let Some(f) = files.iter().find(|f| f.file == query || stem(f) == query || f.name.as_deref() == Some(query)) {
        return Ok(f.clone());
    }
    let hits: Vec<&MemoryFile> = files.iter().filter(|f| f.file.starts_with(query) || f.name.as_deref().is_some_and(|n| n.starts_with(query))).collect();
    match hits.as_slice() {
        [] => bail!("no memory file matches '{query}' in {}", dir_of(project_dir).display()),
        [one] => Ok((*one).clone()),
        many => {
            let names: Vec<&str> = many.iter().take(5).map(|f| f.file.as_str()).collect();
            bail!("'{query}' matches {} memory files ({}{}); use more characters", many.len(), names.join(", "), if many.len() > 5 { ", ..." } else { "" })
        }
    }
}

/// True if `line` links to memory file `file` (`](file)` or `](./file)`).
fn links_to(line: &str, file: &str) -> bool {
    line.contains(&format!("]({file})")) || line.contains(&format!("](./{file})"))
}

/// `text` without the index lines that link to any of `files`, and how many went.
fn drop_index_lines(text: &str, files: &[String]) -> (String, usize) {
    let mut dropped = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if files.iter().any(|f| links_to(line, f)) {
            dropped += 1;
        } else {
            out.push_str(line);
        }
    }
    (out, dropped)
}

#[derive(Debug, Default)]
pub struct Changed {
    pub files: usize,
    pub index_lines: usize,
    pub backup: Option<PathBuf>,
}

/// Rewrites `MEMORY.md` with `text`, backing the old one up first.
fn write_index(index: &Path, text: &str) -> Result<Option<PathBuf>> {
    let bak = if index.is_file() { Some(backup(index)?) } else { None };
    fs::write(index, text).with_context(|| format!("cannot write {}", index.display()))?;
    Ok(bak)
}

/// Deletes these memory files and their lines in `MEMORY.md` (which is backed up first).
pub fn remove(project_dir: &Path, files: &[MemoryFile]) -> Result<Changed> {
    let mem = dir_of(project_dir);
    let mut done = Changed::default();
    let names: Vec<String> = files.iter().filter(|f| !f.is_index()).map(|f| f.file.clone()).collect();
    let index = mem.join(INDEX);
    if !names.is_empty() && index.is_file() && !files.iter().any(MemoryFile::is_index) {
        let (text, n) = drop_index_lines(&fs::read_to_string(&index)?, &names);
        if n > 0 {
            done.backup = write_index(&index, &text)?;
            done.index_lines = n;
        }
    }
    for f in files {
        if f.path.parent() != Some(mem.as_path()) {
            bail!("{} is not inside {}", f.path.display(), mem.display());
        }
        fs::remove_file(&f.path).with_context(|| format!("cannot delete {}", f.path.display()))?;
        done.files += 1;
    }
    Ok(done)
}

/// Copies memory files into `dst_dir`'s memory folder and adds their index lines to its
/// `MEMORY.md` (taken from the source index, or made up from the frontmatter).
pub fn copy(src_dir: &Path, dst_dir: &Path, files: &[MemoryFile], overwrite: bool) -> Result<Changed> {
    let dst_mem = dir_of(dst_dir);
    let files: Vec<&MemoryFile> = files.iter().filter(|f| !f.is_index()).collect();
    if !overwrite {
        if let Some(f) = files.iter().find(|f| dst_mem.join(&f.file).exists()) {
            bail!("{} already exists; pass --force to overwrite", dst_mem.join(&f.file).display());
        }
    }
    fs::create_dir_all(&dst_mem)?;
    let src_index = fs::read_to_string(dir_of(src_dir).join(INDEX)).unwrap_or_default();
    let dst_path = dst_mem.join(INDEX);
    let old_index = fs::read_to_string(&dst_path).unwrap_or_default();
    let mut index = old_index.clone();
    let mut done = Changed::default();
    for f in &files {
        fs::copy(&f.path, dst_mem.join(&f.file)).with_context(|| format!("cannot copy {}", f.path.display()))?;
        done.files += 1;
        if index.lines().any(|l| links_to(l, &f.file)) {
            continue;
        }
        let line = src_index.lines().find(|l| links_to(l, &f.file)).map(str::to_string).unwrap_or_else(|| {
            let title = f.name.clone().unwrap_or_else(|| f.file.trim_end_matches(".md").to_string());
            match &f.description {
                Some(d) => format!("- [{title}]({}) — {d}", f.file),
                None => format!("- [{title}]({})", f.file),
            }
        });
        if !index.is_empty() && !index.ends_with('\n') {
            index.push('\n');
        }
        index.push_str(&line);
        index.push('\n');
        done.index_lines += 1;
    }
    if index != old_index {
        done.backup = write_index(&dst_path, &index)?;
    }
    Ok(done)
}

/// `$VISUAL`, then `$EDITOR`, then `vi` (`notepad` on Windows). May hold arguments (`code -w`).
fn editor() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "vi".into() })
}

/// Opens `path` in the user's editor and waits for it to close.
pub fn open_in_editor(path: &Path) -> Result<()> {
    let ed = editor();
    let mut parts = ed.split_whitespace();
    let prog = parts.next().context("no editor set")?;
    let status = Command::new(prog).args(parts).arg(path).status().with_context(|| format!("cannot start editor '{ed}' (set $EDITOR)"))?;
    if !status.success() {
        bail!("editor '{ed}' exited with {status}");
    }
    Ok(())
}

/// All memory files of a project as one Markdown document.
pub fn to_markdown(cwd: &Path, files: &[MemoryFile]) -> String {
    let mut out = format!("# Memory: {}\n", cwd.display());
    for f in files {
        let text = fs::read_to_string(&f.path).unwrap_or_default();
        out.push_str(&format!("\n## {}\n\n{}", f.file, text));
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub fn to_json(cwd: &Path, files: &[MemoryFile]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = files
        .iter()
        .map(|f| {
            let mut v = serde_json::to_value(f).unwrap_or_default();
            v["content"] = fs::read_to_string(&f.path).unwrap_or_default().into();
            v
        })
        .collect();
    serde_json::json!({ "directory": cwd, "files": items })
}

/// The `autoMemoryDirectory` of the user settings, if set: memory then lives there, not
/// under `projects/`.
pub fn relocated(claude_dir: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).ok()?).ok()?;
    v.get("autoMemoryDirectory")?.as_str().map(str::to_string)
}

fn note_relocated(claude_dir: &Path) {
    if let Some(d) = relocated(claude_dir) {
        println!("{} autoMemoryDirectory is set to {d}; only the memory under {} is managed here", style::bold_cyan("note:"), projects_root(claude_dir).display());
    }
}

/// The git repository root at or above `p`, which is what Claude keys memory by.
fn git_root(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// The project whose memory applies to `path`: its git root's, else the nearest
/// directory at or above it that has memory.
pub fn resolve_project(claude_dir: &Path, path: &Path) -> Result<Project> {
    let path = resolve(path)?;
    let projects: Vec<Project> = list_all_projects(claude_dir)?.into_iter().filter(|p| count(&p.dir) > 0).collect();
    if let Some(root) = git_root(&path) {
        if let Some(p) = projects.iter().find(|p| p.cwd == root) {
            return Ok(p.clone());
        }
    }
    projects
        .into_iter()
        .filter(|p| path.starts_with(&p.cwd))
        .max_by_key(|p| p.cwd.components().count())
        .with_context(|| format!("no auto-memory found for {} or the directories above it", path.display()))
}

/// The project folder memory copied to `path` should land in (it may not exist yet).
fn target_project(claude_dir: &Path, path: &Path) -> Result<Project> {
    let path = resolve(path)?;
    let home = git_root(&path).unwrap_or(path);
    if let Some(p) = list_all_projects(claude_dir)?.into_iter().find(|p| p.cwd == home) {
        return Ok(p);
    }
    Ok(Project { dir: projects_root(claude_dir).join(encode_path(&home)), cwd: home })
}

fn kb(bytes: u64) -> String {
    format!("{:.1} KB", bytes as f64 / 1024.0)
}

fn print_project(p: &Project, files: &[MemoryFile]) {
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    println!(
        "{} {}",
        style::bold_cyan(&p.cwd.display().to_string()),
        style::dim(&format!("({} file(s), {})", files.len(), kb(total)))
    );
    let w = files.iter().map(|f| f.file.chars().count()).max().unwrap_or(0);
    for f in files {
        let kind = if f.is_index() { "index".to_string() } else { f.kind.clone().unwrap_or_default() };
        println!(
            "  {}  {}  {}",
            style::yellow(&format!("{:<w$}", f.file)),
            style::dim(&format!("{kind:<9}")),
            f.summary()
        );
    }
}

/// `memory [PATH] [--all] [--json]`.
pub fn run_list(claude_dir: &Path, path: Option<&Path>, all: bool, json: bool) -> Result<()> {
    let projects: Vec<Project> = if all {
        list_all_projects(claude_dir)?.into_iter().filter(|p| count(&p.dir) > 0).collect()
    } else {
        vec![resolve_project(claude_dir, path.unwrap_or(Path::new(".")))?]
    };
    if json {
        let v: Vec<serde_json::Value> = projects.iter().map(|p| serde_json::json!({ "directory": p.cwd, "folder": p.dir, "files": list(&p.dir) })).collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    note_relocated(claude_dir);
    if projects.is_empty() {
        println!("{}", style::dim("(no auto-memory found)"));
    }
    for (i, p) in projects.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_project(p, &list(&p.dir));
    }
    Ok(())
}

fn pick(claude_dir: &Path, path: Option<&Path>, name: Option<&str>) -> Result<(Project, MemoryFile)> {
    let p = resolve_project(claude_dir, path.unwrap_or(Path::new(".")))?;
    let f = find(&p.dir, name.unwrap_or(INDEX))?;
    Ok((p, f))
}

/// `memory show [NAME]`.
pub fn run_show(claude_dir: &Path, path: Option<&Path>, name: Option<&str>) -> Result<()> {
    let (_, f) = pick(claude_dir, path, name)?;
    print!("{}", fs::read_to_string(&f.path).with_context(|| format!("cannot read {}", f.path.display()))?);
    Ok(())
}

/// `memory edit [NAME]`.
pub fn run_edit(claude_dir: &Path, path: Option<&Path>, name: Option<&str>) -> Result<()> {
    let (_, f) = pick(claude_dir, path, name)?;
    open_in_editor(&f.path)
}

/// Refuses while Claude Code runs in (or below) the project's directory.
pub fn check_running(claude_dir: &Path, p: &Project) -> Result<()> {
    let running = running_claudes(claude_dir);
    if let Some(r) = conflicts(&running, &[p.cwd.as_path()]).first() {
        bail!("Claude Code (pid {}) is running in {}; exit it first", r.pid, r.cwd.display());
    }
    Ok(())
}

pub struct RmOpts<'a> {
    pub path: Option<&'a Path>,
    pub names: &'a [String],
    pub dry_run: bool,
    pub yes: bool,
    pub force: bool,
}

/// `memory rm NAME...`.
pub fn run_rm(claude_dir: &Path, o: &RmOpts) -> Result<()> {
    let p = resolve_project(claude_dir, o.path.unwrap_or(Path::new(".")))?;
    let mut seen = HashSet::new();
    let files: Vec<MemoryFile> = o.names.iter().map(|n| find(&p.dir, n)).collect::<Result<Vec<_>>>()?.into_iter().filter(|f| seen.insert(f.file.clone())).collect();
    println!("{} {}", style::bold_cyan("Delete memory of"), style::cyan(&p.cwd.display().to_string()));
    for f in &files {
        println!("  {}  {}", style::yellow(&f.file), style::dim(&f.summary()));
    }
    if files.iter().any(MemoryFile::is_index) {
        println!("  {}", style::yellow("MEMORY.md is the index Claude loads every session; the other files stay but are no longer listed"));
    }
    if o.dry_run {
        println!("{} nothing deleted.", style::bold_green("Dry run:"));
        return Ok(());
    }
    if !o.force {
        check_running(claude_dir, &p).map_err(|e| anyhow::anyhow!("{e:#} (or pass --force)"))?;
    }
    if !o.yes && !crate::rm::confirm(&format!("Delete {} memory file(s)?", files.len()))? {
        println!("Aborted.");
        return Ok(());
    }
    let d = remove(&p.dir, &files)?;
    println!("{}", style::green(&format!("✔ Deleted {} file(s), {} index line(s) removed.", d.files, d.index_lines)));
    if let Some(b) = d.backup {
        println!("  {} {}", style::dim("MEMORY.md backup:"), b.display());
    }
    Ok(())
}

pub struct CpOpts<'a> {
    pub src: &'a Path,
    pub dst: &'a Path,
    pub names: &'a [String],
    pub dry_run: bool,
    pub force: bool,
}

/// `memory cp SRC DST [NAME...]`.
pub fn run_cp(claude_dir: &Path, o: &CpOpts) -> Result<()> {
    let from = resolve_project(claude_dir, o.src)?;
    let to = target_project(claude_dir, o.dst)?;
    if from.dir == to.dir {
        bail!("source and destination share the same memory folder ({})", from.dir.display());
    }
    let files: Vec<MemoryFile> = if o.names.is_empty() {
        list(&from.dir).into_iter().filter(|f| !f.is_index()).collect()
    } else {
        o.names.iter().map(|n| find(&from.dir, n)).collect::<Result<_>>()?
    };
    println!(
        "{} {} {} {}",
        style::bold_cyan("Copy memory:"),
        style::cyan(&from.cwd.display().to_string()),
        style::dim("→"),
        style::green(&to.cwd.display().to_string())
    );
    for f in &files {
        let clash = dir_of(&to.dir).join(&f.file).exists();
        println!("  {}{}", style::yellow(&f.file), if clash { style::red("  (exists)") } else { String::new() });
    }
    if o.dry_run {
        println!("{} nothing copied.", style::bold_green("Dry run:"));
        return Ok(());
    }
    let d = copy(&from.dir, &to.dir, &files, o.force)?;
    println!("{}", style::green(&format!("✔ Copied {} file(s), {} index line(s) added.", d.files, d.index_lines)));
    Ok(())
}

/// `memory export [PATH]`: the text for `--format md` or `json`.
pub fn export_text(claude_dir: &Path, path: Option<&Path>, json: bool) -> Result<String> {
    let p = resolve_project(claude_dir, path.unwrap_or(Path::new(".")))?;
    let files = list(&p.dir);
    Ok(if json { format!("{}\n", serde_json::to_string_pretty(&to_json(&p.cwd, &files))?) } else { to_markdown(&p.cwd, &files) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tmp: &Path) -> PathBuf {
        let proj = tmp.join("projects/-p");
        let mem = dir_of(&proj);
        fs::create_dir_all(&mem).unwrap();
        fs::write(mem.join(INDEX), "- [Alpha](alpha.md) — first\n- [Beta](./beta.md) — second\n").unwrap();
        fs::write(mem.join("alpha.md"), "---\nname: alpha-one\ndescription: \"the first\"\nmetadata:\n  type: feedback\n---\n\nbody\n").unwrap();
        fs::write(mem.join("beta.md"), "no frontmatter\n").unwrap();
        fs::write(mem.join("betamax.md"), "---\nname: bm\n---\n").unwrap();
        proj
    }

    #[test]
    fn lists_index_first_and_reads_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = setup(tmp.path());
        let files = list(&proj);
        let names: Vec<&str> = files.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(names, ["MEMORY.md", "alpha.md", "beta.md", "betamax.md"]);
        let a = &files[1];
        assert_eq!((a.name.as_deref(), a.description.as_deref(), a.kind.as_deref()), (Some("alpha-one"), Some("the first"), Some("feedback")));
        assert_eq!(files[2].name, None);
        assert_eq!(count(&proj), 4);
    }

    #[test]
    fn find_by_file_stem_name_or_unique_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = setup(tmp.path());
        assert_eq!(find(&proj, "alpha.md").unwrap().file, "alpha.md");
        assert_eq!(find(&proj, "alpha").unwrap().file, "alpha.md");
        assert_eq!(find(&proj, "alpha-one").unwrap().file, "alpha.md");
        assert_eq!(find(&proj, "beta").unwrap().file, "beta.md", "an exact stem beats a prefix");
        assert_eq!(find(&proj, "bm").unwrap().file, "betamax.md");
        assert!(find(&proj, "bet").unwrap_err().to_string().contains("matches 2"));
        for bad in ["../x", "a/b", "", "a\\b", ".."] {
            assert!(find(&proj, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn remove_drops_files_and_their_index_lines_with_a_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = setup(tmp.path());
        let d = remove(&proj, &[find(&proj, "beta").unwrap()]).unwrap();
        assert_eq!((d.files, d.index_lines), (1, 1));
        assert!(d.backup.as_ref().unwrap().is_file());
        assert_eq!(fs::read_to_string(dir_of(&proj).join(INDEX)).unwrap(), "- [Alpha](alpha.md) — first\n");
        assert!(!dir_of(&proj).join("beta.md").exists());
    }

    #[test]
    fn copy_adds_index_lines_and_refuses_to_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let src = setup(tmp.path());
        let dst = tmp.path().join("projects/-q");
        let files: Vec<MemoryFile> = list(&src).into_iter().filter(|f| f.file != "betamax.md").collect();
        let d = copy(&src, &dst, &files, false).unwrap();
        assert_eq!((d.files, d.index_lines, d.backup.is_none()), (2, 2, true));
        let idx = fs::read_to_string(dir_of(&dst).join(INDEX)).unwrap();
        assert_eq!(idx, "- [Alpha](alpha.md) — first\n- [Beta](./beta.md) — second\n");
        assert!(copy(&src, &dst, &files, false).unwrap_err().to_string().contains("--force"));

        let bm = vec![find(&src, "bm").unwrap()];
        let d = copy(&src, &dst, &bm, false).unwrap();
        assert_eq!(d.index_lines, 1);
        assert!(d.backup.is_some());
        assert!(fs::read_to_string(dir_of(&dst).join(INDEX)).unwrap().ends_with("- [bm](betamax.md)\n"));
    }
}
