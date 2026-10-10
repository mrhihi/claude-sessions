use std::fs;
use std::path::{Path, PathBuf};

use claude_sessions::{cp, encode::encode_path, memory, mv, report};

fn session(claude: &Path, cwd: &Path, id: &str) {
    let dir = claude.join("projects").join(encode_path(cwd));
    fs::create_dir_all(&dir).unwrap();
    let line = format!(
        r#"{{"type":"user","cwd":{},"timestamp":"2026-01-01T00:00:00Z","message":{{"content":"hi"}}}}"#,
        serde_json::to_string(&cwd.to_string_lossy()).unwrap()
    );
    fs::write(dir.join(format!("{id}.jsonl")), line + "\n").unwrap();
}

#[test]
fn mv_carries_sessions_of_dir_and_subdirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let claude = root.join("claude");
    let src = root.join("my proj");
    let sub = src.join("sub.dir");
    let dst = root.join("moved/new_name");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("f.txt"), "x").unwrap();
    session(&claude, &src, "s1");
    session(&claude, &sub, "s2");
    session(&claude, &src.join(".git"), "s3");
    fs::write(
        claude.join("history.jsonl"),
        format!("{{\"project\":{},\"sessionId\":\"s1\"}}\n", serde_json::to_string(&src.to_string_lossy()).unwrap()),
    )
    .unwrap();

    let opts = |dry_run| mv::Opts {
        claude_dir: claude.clone(),
        src: src.clone(),
        dst: dst.clone(),
        dry_run,
        no_move_files: false,
        force: false,
    };
    mv::run(&opts(true)).unwrap();
    assert!(src.exists() && !dst.exists());

    let ex = vec![".git".to_string()];
    let before = report::build(&claude, &src, &ex).unwrap();
    assert_eq!(before.total.sessions, 2); // .git excluded

    mv::run(&opts(false)).unwrap();
    assert!(!src.exists());
    assert!(dst.join("sub.dir/f.txt").exists());

    assert_eq!(report::build(&claude, &src, &ex).unwrap().total.sessions, 0);
    let after = report::build(&claude, &dst, &ex).unwrap();
    assert_eq!(after.total.sessions, 2);
    assert_eq!(report::build(&claude, &dst, &[]).unwrap().total.sessions, 3); // .git session moved too
    assert!(claude.join("projects").join(encode_path(&dst)).is_dir());
    let hist = fs::read_to_string(claude.join("history.jsonl")).unwrap();
    assert!(hist.contains("new_name"));
}

fn lstart(pid: u32) -> String {
    let o = std::process::Command::new("ps").env("TZ", "UTC").args(["-o", "lstart=", "-p", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn fake_claude(claude: &Path, pid: u32, cwd: &Path, proc_start: &str) {
    let dir = claude.join("sessions");
    fs::create_dir_all(&dir).unwrap();
    let body = serde_json::json!({"pid": pid, "cwd": cwd, "procStart": proc_start, "name": "fake"});
    fs::write(dir.join(format!("{pid}.json")), body.to_string()).unwrap();
}

#[test]
#[cfg(unix)] // detecting running claude processes relies on ps/pgrep
fn mv_refuses_when_claude_runs_inside_unless_forced_or_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let claude = root.join("claude");
    let src = root.join("proj");
    let dst = root.join("proj2");
    fs::create_dir_all(src.join("deep")).unwrap();
    session(&claude, &src, "s1");
    let me = std::process::id(); // a live pid standing in for Claude

    let opts = |force| mv::Opts {
        claude_dir: claude.clone(),
        src: src.clone(),
        dst: dst.clone(),
        dry_run: false,
        no_move_files: false,
        force,
    };

    // stale file: recorded start time doesn't match the live pid -> ignored, move succeeds
    fake_claude(&claude, me, &src.join("deep"), "Mon Jan  1 00:00:00 2001");
    assert!(mv::running_claudes(&claude).iter().all(|r| r.pid != me));

    // live: matching start time, cwd inside src -> refused, nothing moved
    fake_claude(&claude, me, &src.join("deep"), &lstart(me));
    assert!(mv::running_claudes(&claude).iter().any(|r| r.pid == me));
    assert!(mv::run(&opts(false)).is_err());
    assert!(src.exists() && !dst.exists());

    // --force goes through
    mv::run(&opts(true)).unwrap();
    assert!(dst.exists() && !src.exists());
}

fn snapshot(claude: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else if !p.file_name().unwrap().to_string_lossy().contains(".claude-sessions-") {
                out.push((p.strip_prefix(root).unwrap().display().to_string(), fs::read(&p).unwrap()));
            }
        }
    }
    let mut v = Vec::new();
    walk(claude, claude, &mut v);
    v.sort();
    v
}

#[test]
fn mv_makes_own_backups_and_running_it_backwards_restores_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let claude = root.join("claude");
    let (a, sub, b) = (root.join("a dir"), root.join("a dir/sub"), root.join("b"));
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("f.txt"), "x").unwrap();
    session(&claude, &a, "s1");
    session(&claude, &sub, "s2");
    let q = |p: &Path| serde_json::to_string(&p.to_string_lossy()).unwrap();
    fs::write(claude.join("history.jsonl"), format!("{{\"project\":{},\"sessionId\":\"s1\"}}\n", q(&a))).unwrap();
    fs::write(claude.join(".claude.json"), format!("{{\"projects\":{{{}:{{\"x\":1}}}}}}", q(&a))).unwrap();
    let before = snapshot(&claude);

    let run = |src: &Path, dst: &Path| {
        mv::run(&mv::Opts { claude_dir: claude.clone(), src: src.into(), dst: dst.into(), dry_run: false, no_move_files: false, force: false }).unwrap()
    };
    run(&a, &b);
    assert_ne!(snapshot(&claude), before, "the move changed the records");
    let names: Vec<String> = fs::read_dir(&claude).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|n| n.starts_with("history.jsonl.claude-sessions-") && n.ends_with(".bak")), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with(".claude.json.claude-sessions-") && n.ends_with(".bak")), "{names:?}");
    assert!(!names.iter().any(|n| n == "history.jsonl.bak" || n == ".claude.json.bak"), "{names:?}");

    run(&b, &a); // what the printed "undo:" line says to do
    assert_eq!(snapshot(&claude), before, "session files, history and .claude.json are back byte for byte");
    assert!(a.join("sub/f.txt").exists() && !b.exists());
}

#[test]
fn mv_rewrites_every_directory_key_but_not_history_text() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let claude = root.join("claude");
    let (src, dst) = (root.join("old"), root.join("new"));
    fs::create_dir_all(&src).unwrap();
    let dir = claude.join("projects").join(encode_path(&src));
    fs::create_dir_all(&dir).unwrap();
    // Paths as they appear inside JSON text: backslashes escaped, `sep` joins a sub-path.
    let esc = |p: &Path| {
        let q = serde_json::to_string(&p.to_string_lossy()).unwrap();
        q[1..q.len() - 1].to_string()
    };
    let sep = if cfg!(windows) { "\\\\" } else { "/" };
    let s = esc(&src);
    let lines = [
        format!(r#"{{"type":"user","cwd":"{s}","live_cwd":"{s}","projectPath":"{s}"}}"#),
        format!(r#"{{"type":"relocated","relocatedCwd":"{s}{sep}sub","workingDirectory":"{s}","realParentDir":"{s}"}}"#),
        format!(r#"{{"type":"assistant","file_path":"{s}{sep}f.txt","text":"cwd is {s}"}}"#),
    ];
    fs::write(dir.join("s1.jsonl"), lines.join("\n") + "\n").unwrap();

    mv::run(&mv::Opts { claude_dir: claude.clone(), src: src.clone(), dst: dst.clone(), dry_run: false, no_move_files: true, force: false }).unwrap_err(); // dst missing
    fs::create_dir_all(&dst).unwrap();
    mv::run(&mv::Opts { claude_dir: claude.clone(), src: src.clone(), dst: dst.clone(), dry_run: false, no_move_files: true, force: false }).unwrap();

    let text = fs::read_to_string(claude.join("projects").join(encode_path(&dst)).join("s1.jsonl")).unwrap();
    let d = esc(&dst);
    for key in ["cwd", "live_cwd", "projectPath", "workingDirectory", "realParentDir"] {
        assert!(text.contains(&format!(r#""{key}":"{d}""#)), "{key} not rewritten");
    }
    assert!(text.contains(&format!(r#""relocatedCwd":"{d}{sep}sub""#)));
    assert!(text.contains(&format!(r#""file_path":"{s}{sep}f.txt""#)), "tool input must stay");
}

#[test]
fn mv_long_path_uses_claudes_truncated_hashed_folder_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let claude = root.join("claude");
    let src = root.join("a".repeat(120)).join("b".repeat(120));
    let dst = root.join("c".repeat(120)).join("d".repeat(120));
    fs::create_dir_all(&src).unwrap();
    session(&claude, &src, "s1");
    mv::run(&mv::Opts { claude_dir: claude.clone(), src, dst: dst.clone(), dry_run: false, no_move_files: false, force: false }).unwrap();
    let name = encode_path(&dst);
    assert!(name.len() > 200 && claude.join("projects").join(name).join("s1.jsonl").is_file());
}

#[test]
fn resolve_dst_moves_into_existing_directory_like_mv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (src, parent) = (root.join("proj"), root.join("parent"));
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&parent).unwrap();
    assert_eq!(mv::resolve_dst(&src, parent.clone(), false).unwrap(), parent.join("proj"));
    assert_eq!(mv::resolve_dst(&src, parent.clone(), true).unwrap(), parent); // --no-move-files: as given
    let fresh = root.join("fresh");
    assert_eq!(mv::resolve_dst(&src, fresh.clone(), false).unwrap(), fresh); // rename
    // A backslash is only suspicious where it is not the path separator.
    if cfg!(unix) {
        assert!(mv::resolve_dst(&src, PathBuf::from("a\\b"), false).is_err());
    }
}

fn memory_only(claude: &Path, cwd: &Path) -> PathBuf {
    let mem = claude.join("projects").join(encode_path(cwd)).join("memory");
    fs::create_dir_all(&mem).unwrap();
    fs::write(mem.join("MEMORY.md"), "- [Note](note.md) — a note\n").unwrap();
    fs::write(mem.join("note.md"), "---\nname: note\n---\nremember this\n").unwrap();
    mem
}

#[test]
fn mv_and_cp_carry_a_memory_only_project_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let claude = root.join("claude");
    let (src, moved, copied) = (root.join("repo"), root.join("repo2"), root.join("repo3"));
    fs::create_dir_all(src.join(".git")).unwrap();
    session(&claude, &src.join("sub"), "s1");
    let old_mem = memory_only(&claude, &src);

    mv::run(&mv::Opts { claude_dir: claude.clone(), src: src.clone(), dst: moved.clone(), dry_run: false, no_move_files: false, force: false }).unwrap();
    assert!(!old_mem.exists());
    let new_mem = claude.join("projects").join(encode_path(&moved)).join("memory");
    assert!(new_mem.join("note.md").is_file());
    assert_eq!(memory::resolve_project(&claude, &moved.join("sub")).unwrap().cwd, moved);
    assert_eq!(report::build(&claude, &moved, &[]).unwrap().memory_files, 2);

    cp::run(&cp::Opts { claude_dir: claude.clone(), src: moved.clone(), dst: copied.clone(), dry_run: false, no_copy_files: false }).unwrap();
    assert!(new_mem.join("note.md").is_file());
    assert!(claude.join("projects").join(encode_path(&copied)).join("memory/note.md").is_file());
}
