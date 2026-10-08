use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::mv::claude_json_path;
use crate::scan::session_files;

/// Ids of the sessions in one `projects/<encoded>` folder: transcripts plus folders
/// named after a session (`subagents/`, `tool-results/`). `memory/` belongs to the
/// project, not to a session.
pub fn session_ids(project_dir: &Path) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for f in session_files(project_dir) {
        if let Some(s) = f.file_stem() {
            ids.insert(s.to_string_lossy().into_owned());
        }
    }
    for e in fs::read_dir(project_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name != "memory" && e.path().is_dir() {
            ids.insert(name);
        }
    }
    ids.into_iter().collect()
}

/// Data kept outside the project folder for one session. Only paths that exist are
/// returned. `plans/` and `shell-snapshots/` are deliberately absent: nothing ties
/// them to a session id.
pub fn sidecar_paths(claude_dir: &Path, id: &str) -> Vec<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
        return vec![];
    }
    [
        format!("file-history/{id}"),
        format!("session-env/{id}"),
        format!("tasks/{id}"),
        format!("debug/{id}"),
        format!("debug/{id}.txt"),
    ]
    .iter()
    .map(|p| claude_dir.join(p))
    .filter(|p| p.exists())
    .collect()
}

/// Every session id of every folder under `projects/`, including folders whose working
/// directory can't be worked out (those are invisible to `scan::list_projects`).
pub fn all_session_ids(claude_dir: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    for e in fs::read_dir(crate::scan::projects_root(claude_dir)).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            ids.extend(session_ids(&e.path()));
        }
    }
    ids
}

/// Session ids recorded in `sessions/*.json` (live or stale; stale ones only make callers more careful).
pub fn running_session_ids(claude_dir: &Path) -> HashSet<String> {
    fs::read_dir(claude_dir.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .filter_map(|v| v.get("sessionId").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// `8-4-4-4-12` hex digits, the shape of a session id. Used so that only entries which
/// really look like per-session data are ever treated as such.
pub fn looks_like_session_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5 && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Number of regular files below `p`.
pub fn count_files(p: &Path) -> usize {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| count_files(&e.path())).sum(),
        Ok(_) => 1,
        Err(_) => 0,
    }
}

pub fn path_size(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| path_size(&e.path())).sum(),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

pub fn remove_path(p: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if meta.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
    .with_context(|| format!("cannot delete {}", p.display()))
}

/// A planned rewrite of `history.jsonl` or `~/.claude.json`.
pub struct Edit {
    pub path: PathBuf,
    pub text: String,
    /// Lines (history) or project entries (`.claude.json`) removed.
    pub removed: usize,
    /// For `.claude.json`: the project paths that were removed.
    pub keys: Vec<String>,
}

/// Drops every `history.jsonl` line for which `drop` is true; lines that don't parse are kept.
pub fn plan_history(claude_dir: &Path, drop: impl Fn(&Value) -> bool) -> Result<Option<Edit>> {
    let path = claude_dir.join("history.jsonl");
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let (mut out, mut removed) = (String::with_capacity(text.len()), 0);
    for line in text.split_inclusive('\n') {
        match serde_json::from_str::<Value>(line.trim_end()) {
            Ok(v) if drop(&v) => removed += 1,
            _ => out.push_str(line),
        }
    }
    Ok((removed > 0).then_some(Edit { path, text: out, removed, keys: vec![] }))
}

/// Removes `projects.<path>` entries of `~/.claude.json` whose path satisfies `remove`.
/// Text-level, so the rest of the file keeps its exact bytes. Returns `None` when
/// nothing matches, and an error when the file can't be edited safely.
pub fn plan_claude_json(claude_dir: &Path, remove: impl Fn(&str) -> bool) -> Result<Option<Edit>> {
    let Some(path) = claude_json_path(claude_dir) else { return Ok(None) };
    let text = fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let (out, keys) = remove_project_entries(&text, remove)?;
    Ok((!keys.is_empty()).then_some(Edit { path, removed: keys.len(), text: out, keys }))
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// End (exclusive) of the string token starting at `i` (which is a quote).
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

/// End (exclusive) of the JSON value starting at `i`.
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match *b.get(i)? {
        b'"' => string_end(b, i),
        b'{' | b'[' => {
            let (mut depth, mut j) = (0usize, i);
            while j < b.len() {
                match b[j] {
                    b'"' => j = string_end(b, j)? - 1,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            Some(j)
        }
    }
}

struct Entry {
    key: String,
    start: usize,
    end: usize,
}

/// The `"key": value` entries of the object whose `{` is at `open`, and the index of its `}`.
fn object_entries(b: &[u8], open: usize) -> Option<(Vec<Entry>, usize)> {
    let mut entries = Vec::new();
    let mut i = skip_ws(b, open + 1);
    loop {
        match *b.get(i)? {
            b'}' => return Some((entries, i)),
            b',' => i = skip_ws(b, i + 1),
            b'"' => {
                let kend = string_end(b, i)?;
                let key = serde_json::from_slice::<String>(&b[i..kend]).ok()?;
                let colon = skip_ws(b, kend);
                if *b.get(colon)? != b':' {
                    return None;
                }
                let vstart = skip_ws(b, colon + 1);
                let end = value_end(b, vstart)?;
                entries.push(Entry { key, start: i, end });
                i = skip_ws(b, end);
            }
            _ => return None,
        }
    }
}

/// Position of the `{` of the top-level `"projects"` object.
fn projects_open(b: &[u8]) -> Option<usize> {
    let root = skip_ws(b, 0);
    if *b.get(root)? != b'{' {
        return None;
    }
    let (entries, _) = object_entries(b, root)?;
    let e = entries.iter().find(|e| e.key == "projects")?;
    let colon = skip_ws(b, e.start + serde_json::to_string(&e.key).ok()?.len());
    let v = skip_ws(b, colon + 1);
    (*b.get(v)? == b'{').then_some(v)
}

pub fn remove_project_entries(text: &str, remove: impl Fn(&str) -> bool) -> Result<(String, Vec<String>)> {
    let b = text.as_bytes();
    let Some(open) = projects_open(b) else { return Ok((text.to_string(), vec![])) };
    let Some((entries, close)) = object_entries(b, open) else {
        anyhow::bail!("cannot parse the projects object of .claude.json; leaving it alone");
    };
    let gone: Vec<bool> = entries.iter().map(|e| remove(&e.key)).collect();
    if !gone.iter().any(|g| *g) {
        return Ok((text.to_string(), vec![]));
    }
    let kept: Vec<usize> = (0..entries.len()).filter(|i| !gone[*i]).collect();
    let mut inner = String::new();
    match kept.first() {
        None => {}
        Some(_) => {
            inner.push_str(&text[open + 1..entries[0].start]);
            for (n, i) in kept.iter().enumerate() {
                inner.push_str(&text[entries[*i].start..entries[*i].end]);
                if n + 1 < kept.len() {
                    // Separator that followed this entry in the original file.
                    inner.push_str(&text[entries[*i].end..entries[*i + 1].start]);
                }
            }
            inner.push_str(&text[entries[entries.len() - 1].end..close]);
        }
    }
    let out = format!("{}{}{}", &text[..open + 1], inner, &text[close..]);
    let removed: Vec<String> = entries.iter().zip(&gone).filter(|(_, g)| **g).map(|(e, _)| e.key.clone()).collect();
    // Never write something we can't read back, or that still has what we meant to drop.
    let check = serde_json::from_str::<Value>(&out).context("edited .claude.json would not be valid JSON; leaving it alone")?;
    if let Some(p) = check.get("projects").and_then(Value::as_object) {
        if removed.iter().any(|k| p.contains_key(k)) {
            anyhow::bail!("could not remove every project entry from .claude.json; leaving it alone");
        }
    }
    Ok((out, removed))
}

/// How many of our backups of one file are kept.
pub const KEEP_BACKUPS: usize = 3;

const TAG: &str = ".claude-sessions-";

/// `(stamp, n)` of a backup of `base` made by this tool, or `None` for any other file
/// (including plain `.bak` files that someone else made).
fn our_backup(name: &str, base: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix(base)?.strip_prefix(TAG)?.strip_suffix(".bak")?;
    let (stamp, n) = match rest.split_once('-') {
        Some((s, n)) => (s, n.parse::<u32>().ok().filter(|n| *n >= 2)?),
        None => (rest, 1),
    };
    let b = stamp.as_bytes();
    let ok = b.len() == 16 && b[8] == b'T' && b[15] == b'Z' && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit);
    ok.then(|| (stamp.to_string(), n))
}

/// Copies `path` to `<name>.claude-sessions-<UTC stamp>.bak` next to it, never overwriting
/// an existing file, then drops all but the newest `KEEP_BACKUPS` of ours. Returns the new
/// backup. Backups made by anything else are never touched.
pub fn backup(path: &Path) -> Result<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let base = path.file_name().context("no file name")?.to_string_lossy().into_owned();
    let stamp = crate::timespec::stamp(crate::timespec::now_secs());
    // Continue after the highest number already used in this second. Reusing a freed
    // name would make a newer backup sort as older and get pruned first.
    let used = backups_of(path)
        .iter()
        .filter_map(|p| our_backup(&p.file_name()?.to_string_lossy(), &base))
        .filter(|(s, _)| *s == stamp)
        .map(|(_, n)| n)
        .max();
    for n in used.map_or(1, |m| m + 1).. {
        let name = if n == 1 { format!("{base}{TAG}{stamp}.bak") } else { format!("{base}{TAG}{stamp}-{n}.bak") };
        let target = dir.join(name);
        match fs::File::create_new(&target) {
            Ok(_) => {
                if let Err(e) = fs::copy(path, &target) {
                    let _ = fs::remove_file(&target);
                    return Err(e).with_context(|| format!("cannot back up {}", path.display()));
                }
                prune_backups(path, KEEP_BACKUPS);
                return Ok(target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("cannot create {}", target.display())),
        }
    }
    unreachable!()
}

/// Our backups of `path`, oldest first.
pub fn backups_of(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let Some(base) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return vec![] };
    let mut found: Vec<((String, u32), PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| our_backup(&e.file_name().to_string_lossy(), &base).map(|k| (k, e.path())))
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

fn prune_backups(path: &Path, keep: usize) {
    let all = backups_of(path);
    for old in all.iter().take(all.len().saturating_sub(keep)) {
        let _ = fs::remove_file(old);
    }
}

/// Backs the file up (see `backup`), then replaces it, keeping permissions and mtime
/// (Claude orders its resume list by mtime). Returns the backup's path.
pub fn apply(e: &Edit) -> Result<PathBuf> {
    let meta = fs::metadata(&e.path)?;
    let bak = backup(&e.path)?;
    let tmp = e.path.with_extension("tmp-claude-sessions");
    fs::write(&tmp, &e.text)?;
    fs::set_permissions(&tmp, meta.permissions())?;
    fs::File::options().write(true).open(&tmp)?.set_modified(meta.modified()?)?;
    fs::rename(&tmp, &e.path)?;
    Ok(bak)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(text: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(text).unwrap();
        let mut k: Vec<String> = v["projects"].as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    const CJ: &str = "{\n  \"a\": 1,\n  \"projects\": {\n    \"/a/b\": {\"x\": {\"y\": \"}\"}},\n    \"/a/bc\": {},\n    \"/q \\\"z\\\"\": [1, 2],\n    \"/last\": {\"p\": \"/a/b\"}\n  },\n  \"z\": 2\n}\n";

    #[test]
    fn removes_first_middle_and_last_entries() {
        for gone in ["/a/b", "/a/bc", "/q \"z\"", "/last"] {
            let (out, removed) = remove_project_entries(CJ, |k| k == gone).unwrap();
            assert_eq!(removed, vec![gone.to_string()]);
            let mut want: Vec<String> = ["/a/b", "/a/bc", "/q \"z\"", "/last"].iter().map(|s| s.to_string()).collect();
            want.retain(|k| k != gone);
            want.sort();
            assert_eq!(keys_of(&out), want, "removing {gone}");
            assert!(out.starts_with("{\n  \"a\": 1,") && out.ends_with("\"z\": 2\n}\n"));
        }
    }

    #[test]
    fn prefix_paths_and_values_are_not_confused() {
        let (out, removed) = remove_project_entries(CJ, |k| k == "/a/b").unwrap();
        assert_eq!(removed.len(), 1);
        assert!(out.contains("\"/a/bc\": {}") && out.contains("\"p\": \"/a/b\""));
    }

    #[test]
    fn removing_all_or_none() {
        let (out, removed) = remove_project_entries(CJ, |_| true).unwrap();
        assert_eq!(removed.len(), 4);
        assert!(keys_of(&out).is_empty());
        let (same, removed) = remove_project_entries(CJ, |_| false).unwrap();
        assert!(removed.is_empty() && same == CJ);
        let (same, removed) = remove_project_entries("{\"a\":1}", |_| true).unwrap();
        assert!(removed.is_empty() && same == "{\"a\":1}");
    }

    #[test]
    fn ignores_projects_keys_that_are_not_top_level() {
        let t = "{\"x\": {\"projects\": {\"/a\": {}}}, \"projects\": {\"/b\": {}}}";
        let (out, removed) = remove_project_entries(t, |_| true).unwrap();
        assert_eq!(removed, vec!["/b".to_string()]);
        assert!(out.contains("\"/a\": {}"));
    }

    #[test]
    fn history_lines_are_filtered_and_junk_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let h = "{\"sessionId\":\"s1\",\"project\":\"/a\"}\nnot json\n{\"sessionId\":\"s2\",\"project\":\"/a\"}\n";
        fs::write(tmp.path().join("history.jsonl"), h).unwrap();
        let e = plan_history(tmp.path(), |v| v["sessionId"] == "s1").unwrap().unwrap();
        assert_eq!(e.removed, 1);
        assert_eq!(e.text, "not json\n{\"sessionId\":\"s2\",\"project\":\"/a\"}\n");
        assert!(plan_history(tmp.path(), |_| false).unwrap().is_none());
        let bak = apply(&e).unwrap();
        assert_eq!(fs::read_to_string(tmp.path().join("history.jsonl")).unwrap(), e.text);
        assert_eq!(fs::read_to_string(&bak).unwrap(), h);
        let name = bak.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("history.jsonl.claude-sessions-") && name.ends_with("Z.bak"), "{name}");
        assert!(!tmp.path().join("history.jsonl.bak").exists(), "no generic .bak any more");
    }

    #[test]
    fn backup_names_are_ours_and_never_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("history.jsonl");
        fs::write(&f, "v1").unwrap();
        let a = backup(&f).unwrap();
        fs::write(&f, "v2").unwrap();
        let b = backup(&f).unwrap();
        assert_ne!(a, b, "same second must not reuse the name");
        assert_eq!(fs::read_to_string(&a).unwrap(), "v1");
        assert_eq!(fs::read_to_string(&b).unwrap(), "v2");
        assert_eq!(backups_of(&f), vec![a, b]);
    }

    #[test]
    fn keeps_only_the_newest_three_and_leaves_foreign_files_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("history.jsonl");
        for foreign in ["history.jsonl.bak", "history.jsonl.bak-1", "history.jsonl.claude-sessions-notastamp.bak", "other.jsonl.claude-sessions-20200101T000000Z.bak"] {
            fs::write(tmp.path().join(foreign), "foreign").unwrap();
        }
        let mut made = Vec::new();
        for i in 0..5 {
            fs::write(&f, format!("v{i}")).unwrap();
            made.push(backup(&f).unwrap());
        }
        let left = backups_of(&f);
        assert_eq!(left, made[2..].to_vec(), "newest three survive");
        let contents: Vec<String> = left.iter().map(|p| fs::read_to_string(p).unwrap()).collect();
        assert_eq!(contents, ["v2", "v3", "v4"]);
        for foreign in ["history.jsonl.bak", "history.jsonl.bak-1", "history.jsonl.claude-sessions-notastamp.bak", "other.jsonl.claude-sessions-20200101T000000Z.bak"] {
            assert!(tmp.path().join(foreign).exists(), "{foreign} must be untouched");
        }
    }

    #[test]
    fn recognizes_only_well_formed_backup_names() {
        let ok = |n: &str| our_backup(n, "history.jsonl").is_some();
        assert!(ok("history.jsonl.claude-sessions-20261008T083342Z.bak"));
        assert!(ok("history.jsonl.claude-sessions-20261008T083342Z-2.bak"));
        assert!(!ok("history.jsonl.claude-sessions-20261008T083342Z-1.bak"));
        assert!(!ok("history.jsonl.claude-sessions-2026100T0833422Z.bak"));
        assert!(!ok("history.jsonl.bak") && !ok("history.jsonl.claude-sessions-20261008T083342Z"));
    }

    #[test]
    fn session_id_shape() {
        assert!(looks_like_session_id("deadbeef-0000-0000-0000-000000000000"));
        assert!(looks_like_session_id("409AA582-1514-444c-b62a-1e1817e3441b"));
        for bad in ["", "plans", "deadbeef-0000-0000-0000-00000000000", "deadbeef-0000-0000-0000-00000000000g", "memory"] {
            assert!(!looks_like_session_id(bad), "{bad}");
        }
    }

    #[test]
    fn finds_session_ids_and_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude, proj) = (tmp.path(), tmp.path().join("projects/p"));
        fs::create_dir_all(proj.join("memory")).unwrap();
        fs::create_dir_all(proj.join("bbb/subagents")).unwrap();
        fs::write(proj.join("aaa.jsonl"), "").unwrap();
        fs::create_dir_all(claude.join("file-history/aaa")).unwrap();
        fs::create_dir_all(claude.join("session-env/aaa")).unwrap();
        fs::create_dir_all(claude.join("plans")).unwrap();
        assert_eq!(session_ids(&proj), vec!["aaa", "bbb"]);
        assert_eq!(sidecar_paths(claude, "aaa").len(), 2);
        assert!(sidecar_paths(claude, "bbb").is_empty());
        assert!(sidecar_paths(claude, "../x").is_empty());
        assert!(sidecar_paths(claude, "").is_empty());
    }
}
