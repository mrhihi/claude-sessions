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
    fn exclusion_matches_any_component() {
        let ex = vec![".git".to_string(), "target".to_string()];
        assert!(is_excluded(Path::new("a/.git/hooks"), &ex));
        assert!(is_excluded(Path::new("target"), &ex));
        assert!(!is_excluded(Path::new("a/gitx"), &ex));
        assert!(!is_excluded(Path::new(""), &ex));
    }
}
