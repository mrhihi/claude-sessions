use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::encode::encode_path;

pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git", "node_modules", "target", ".venv", "venv", "__pycache__", ".idea", ".vscode", "dist",
    "build", ".next", ".cache",
];

/// One folder under `<claude>/projects` and the real directory it belongs to.
#[derive(Debug, Clone)]
pub struct Project {
    pub dir: PathBuf,
    pub cwd: PathBuf,
}

pub fn projects_root(claude_dir: &Path) -> PathBuf {
    claude_dir.join("projects")
}

pub fn list_projects(claude_dir: &Path) -> Result<Vec<Project>> {
    let root = projects_root(claude_dir);
    let mut out = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("cannot read {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(cwd) = detect_cwd(&dir, &name) {
            out.push(Project { dir, cwd });
        }
    }
    out.sort_by(|a, b| a.cwd.cmp(&b.cwd));
    Ok(out)
}

/// `list_projects` plus folders that hold only auto-memory (no transcript to read a
/// `cwd` from). Their directory is looked up in `.claude.json`, `history.jsonl` and
/// finally on disk; folders whose directory can't be found stay invisible.
pub fn list_all_projects(claude_dir: &Path) -> Result<Vec<Project>> {
    let mut out = list_projects(claude_dir)?;
    let known: HashSet<PathBuf> = out.iter().map(|p| p.dir.clone()).collect();
    let mut known_paths = None;
    for entry in fs::read_dir(projects_root(claude_dir))?.flatten() {
        let dir = entry.path();
        if known.contains(&dir) || !dir.is_dir() || crate::memory::count(&dir) == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let paths = known_paths.get_or_insert_with(|| recorded_paths(claude_dir));
        if let Some(cwd) = paths.get(&name).cloned().or_else(|| find_on_disk(&name)) {
            out.push(Project { dir, cwd });
        }
    }
    out.sort_by(|a, b| a.cwd.cmp(&b.cwd));
    Ok(out)
}

/// Every directory recorded in `.claude.json` (project keys) or `history.jsonl`
/// (`project` fields), keyed by its project-folder name.
fn recorded_paths(claude_dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut add = |p: &str| {
        let p = PathBuf::from(p);
        out.entry(encode_path(&p)).or_insert(p);
    };
    if let Some(cj) = crate::mv::claude_json_path(claude_dir) {
        let v: Option<serde_json::Value> = fs::read_to_string(cj).ok().and_then(|t| serde_json::from_str(&t).ok());
        if let Some(projects) = v.as_ref().and_then(|v| v.get("projects")).and_then(|p| p.as_object()) {
            projects.keys().for_each(|k| add(k));
        }
    }
    if let Ok(file) = File::open(claude_dir.join("history.jsonl")) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if let Some(p) = v.get("project").and_then(|p| p.as_str()) {
                add(p);
            }
        }
    }
    out
}

/// Walks down from the filesystem roots, entering only directories whose folder name
/// is a prefix of `name`, until one encodes to exactly `name`.
fn find_on_disk(name: &str) -> Option<PathBuf> {
    let roots: Vec<PathBuf> = if cfg!(windows) {
        (b'A'..=b'Z').map(|d| PathBuf::from(format!("{}:\\", d as char))).collect()
    } else {
        vec![PathBuf::from("/")]
    };
    let mut budget = 20_000;
    roots.iter().find_map(|r| walk_to(r, name, &mut budget))
}

fn walk_to(dir: &Path, name: &str, budget: &mut usize) -> Option<PathBuf> {
    let enc = encode_path(dir);
    if enc == name {
        return Some(dir.to_path_buf());
    }
    // A hashed (over-long) name can't be a prefix; the separator check stops `/a` from matching `/ab`.
    let leads = name.len() > enc.len() && name.starts_with(&enc) && (enc.ends_with('-') || name.as_bytes()[enc.len()] == b'-');
    if !leads || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let mut kids: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    kids.sort();
    kids.iter().find_map(|k| walk_to(k, name, budget))
}

pub fn session_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl") && p.is_file())
        .collect();
    v.sort();
    v
}

/// The session whose id equals or (uniquely) starts with `id`.
pub fn find_session(claude_dir: &Path, id: &str) -> Result<(Project, PathBuf)> {
    let mut hits = Vec::new();
    for p in list_projects(claude_dir)? {
        for f in session_files(&p.dir) {
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if stem == id {
                return Ok((p, f));
            }
            if !id.is_empty() && stem.starts_with(id) {
                hits.push((p.clone(), f, stem));
            }
        }
    }
    match hits.len() {
        0 => anyhow::bail!("no session id starts with '{id}'"),
        1 => {
            let (p, f, _) = hits.remove(0);
            Ok((p, f))
        }
        n => {
            let ids: Vec<&str> = hits.iter().take(5).map(|h| h.2.as_str()).collect();
            anyhow::bail!("'{id}' matches {n} sessions ({}{}); use more characters", ids.join(", "), if n > 5 { ", ..." } else { "" })
        }
    }
}

/// The folder name is lossy, so the real path comes from the `cwd` recorded in the
/// session files. Prefer a `cwd` whose encoding matches the folder name.
fn detect_cwd(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut first = None;
    for f in session_files(dir).iter().take(5) {
        let Ok(file) = File::open(f) else { continue };
        for line in BufReader::new(file).lines().take(100).map_while(Result::ok) {
            if !line.contains("\"cwd\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let Some(c) = v.get("cwd").and_then(|c| c.as_str()) else { continue };
            let c = PathBuf::from(c);
            if encode_path(&c) == name {
                return Some(c);
            }
            first.get_or_insert(c);
        }
    }
    first
}

/// Absolute path with the longest existing prefix canonicalized (so it also works
/// for directories that don't exist yet or any more).
pub fn resolve(p: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(p)?;
    let mut tail = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            out.extend(tail.iter().rev());
            return Ok(out);
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                cur = parent;
            }
            _ => return Ok(abs),
        }
    }
}

/// `path` relative to `base`. Falls back to a case-insensitive match when the two
/// spellings reach the same directory (case-insensitive filesystems record the same
/// folder as `gsscli` and `GSSCLI`).
pub fn strip_prefix_ci(path: &Path, base: &Path) -> Option<PathBuf> {
    if let Ok(r) = path.strip_prefix(base) {
        return Some(r.to_path_buf());
    }
    let lower = |c: std::path::Component| c.as_os_str().to_string_lossy().to_lowercase();
    let n = base.components().count();
    let mut pc = path.components();
    let head: PathBuf = pc.by_ref().take(n).collect();
    if head.components().count() != n || !head.components().map(lower).eq(base.components().map(lower)) {
        return None;
    }
    match (head.canonicalize(), base.canonicalize()) {
        (Ok(a), Ok(b)) if a == b => Some(pc.collect()),
        _ => None,
    }
}

/// True if any component of `rel` is in the exclude list.
pub fn is_excluded(rel: &Path, excludes: &[String]) -> bool {
    rel.components()
        .any(|c| excludes.iter().any(|e| c.as_os_str() == e.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prefix_ci_folds_case_only_for_the_same_directory() {
        let t = std::env::temp_dir().join(format!("cs-ci-{}", std::process::id()));
        std::fs::create_dir_all(t.join("Proj/sub")).unwrap();
        let up = t.join("PROJ");
        let exact = t.join("Proj");
        assert_eq!(strip_prefix_ci(&exact.join("sub"), &exact), Some(PathBuf::from("sub")));
        assert_eq!(strip_prefix_ci(&t.join("Proj2"), &exact), None);
        let ci = up.exists(); // case-insensitive filesystem?
        assert_eq!(strip_prefix_ci(&up.join("sub"), &exact), ci.then(|| PathBuf::from("sub")));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn memory_only_folders_are_listed_with_their_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let claude = root.join(".claude");
        let (on_disk, in_json, in_history, unknown) =
            (root.join("my proj.v2/sub"), root.join("gone a"), root.join("gone b"), root.join("gone c"));
        fs::create_dir_all(&on_disk).unwrap();
        for p in [&on_disk, &in_json, &in_history, &unknown] {
            let mem = claude.join("projects").join(encode_path(p)).join("memory");
            fs::create_dir_all(&mem).unwrap();
            fs::write(mem.join("MEMORY.md"), "- [x](x.md)\n").unwrap();
        }
        let q = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
        fs::write(claude.join(".claude.json"), format!("{{\"projects\":{{{}:{{}}}}}}", q(&in_json))).unwrap();
        fs::write(claude.join("history.jsonl"), format!("{{\"project\":{}}}\nnot json\n", q(&in_history))).unwrap();
        // An empty memory folder is not worth listing.
        fs::create_dir_all(claude.join("projects/-empty/memory")).unwrap();

        let cwds: Vec<PathBuf> = list_all_projects(&claude).unwrap().into_iter().map(|p| p.cwd).collect();
        assert_eq!(cwds.len(), 3, "{cwds:?}");
        for p in [&on_disk, &in_json, &in_history] {
            assert!(cwds.contains(p), "{} missing from {cwds:?}", p.display());
        }
        assert!(list_projects(&claude).unwrap().is_empty());
    }

    #[test]
    fn exclusion_matches_any_component() {
        let ex = vec![".git".to_string(), "target".to_string()];
        assert!(is_excluded(Path::new("a/.git/hooks"), &ex));
        assert!(is_excluded(Path::new("target"), &ex));
        assert!(!is_excluded(Path::new("a/gitx"), &ex));
        assert!(!is_excluded(Path::new(""), &ex));
    }
}
