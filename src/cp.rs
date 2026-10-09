use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::mv::{jsonl_files_recursive, plan_steps, rewrite_session_file};
use crate::scan::{resolve, session_files};
use crate::style;

pub struct Opts {
    pub claude_dir: PathBuf,
    pub src: PathBuf,
    pub dst: PathBuf,
    pub dry_run: bool,
    /// The directory was already copied by hand; only copy the sessions.
    pub no_copy_files: bool,
}

/// Recursively copies `src` to `dst`, keeping file permissions, modification times and
/// (on unix) symlinks, like `cp -a`.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_symlink() {
            copy_symlink(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
            fs::File::options().write(true).open(&to)?.set_modified(fs::metadata(&from)?.modified()?)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    Ok(())
}

/// Creating symlinks needs elevated rights on Windows, so copy what the link points at.
#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() { copy_tree(from, to) } else { fs::copy(from, to).map(|_| ()).map_err(Into::into) }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(e) = copy_tree(src, dst) {
        let _ = fs::remove_dir_all(dst);
        bail!("copying {} -> {} failed: {e}", src.display(), dst.display());
    }
    Ok(())
}

fn folder(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Copies a directory together with its Claude sessions (and those of its
/// subdirectories). The originals are never touched; `history.jsonl` and
/// `.claude.json` are left alone, so Claude will ask to trust the new directory.
pub fn run(o: &Opts) -> Result<()> {
    let src = resolve(&o.src)?;
    let dst = resolve(&o.dst)?;
    if src == dst {
        bail!("source and destination are the same");
    }
    if dst.starts_with(&src) {
        bail!("destination is inside the source directory");
    }
    if o.no_copy_files {
        if !dst.is_dir() {
            bail!("--no-copy-files: {} does not exist", dst.display());
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
    if let Some(s) = steps.iter().find(|s| s.new_dir == s.project.dir) {
        bail!("session folder {} would be copied onto itself", s.new_dir.display());
    }

    println!(
        "{} {} {} {}",
        style::bold_cyan(if o.no_copy_files { "Copy sessions only:" } else { "Copy:" }),
        style::cyan(&src.display().to_string()),
        style::dim("→"),
        style::green(&dst.display().to_string())
    );
    for s in &steps {
        println!(
            "  {}  {} {} {}",
            style::yellow(&format!("{} session(s)", session_files(&s.project.dir).len())),
            style::dim(&folder(&s.project.dir)),
            style::dim("→"),
            style::green(&folder(&s.new_dir))
        );
    }
    if steps.is_empty() {
        println!("  {}", style::dim("(no Claude sessions found for this directory)"));
    }
    if o.dry_run {
        println!("{} nothing copied.", style::bold_green("Dry run:"));
        return Ok(());
    }

    if !o.no_copy_files {
        copy_dir(&src, &dst)?;
        println!("{}", style::green("✔ Copied directory."));
    }
    let (old, new) = (src.to_string_lossy().into_owned(), dst.to_string_lossy().into_owned());
    let mut rewritten = 0;
    for s in &steps {
        copy_dir(&s.project.dir, &s.new_dir)?;
        let mut files = Vec::new();
        jsonl_files_recursive(&s.new_dir, &mut files);
        for f in files {
            // Only the copy is rewritten, so the original sessions keep pointing at `src`.
            rewritten += match rewrite_session_file(&f, &old, &new) {
                Ok(n) => n,
                Err(e) => {
                    let _ = fs::remove_dir_all(&s.new_dir);
                    return Err(e);
                }
            };
        }
    }
    println!("{}", style::green(&format!("✔ Copied {} session folder(s), {} cwd record(s) rewritten.", steps.len(), rewritten)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::encode_path;

    fn cwd_of(session: &Path) -> String {
        let v: serde_json::Value = serde_json::from_str(fs::read_to_string(session).unwrap().trim()).unwrap();
        v["cwd"].as_str().unwrap().to_string()
    }

    #[test]
    fn copies_dir_and_sessions_without_touching_originals() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (src, sub, dst) = (root.join("a"), root.join("a/sub"), root.join("b"));
        fs::create_dir_all(&sub).unwrap();
        fs::write(src.join("f.txt"), "x").unwrap();
        let claude = root.join(".claude");
        for cwd in [&src, &sub] {
            let dir = claude.join("projects").join(encode_path(cwd));
            fs::create_dir_all(&dir).unwrap();
            let c = serde_json::to_string(&cwd.display().to_string()).unwrap();
            fs::write(dir.join("s.jsonl"), format!("{{\"cwd\":{c}}}\n")).unwrap();
        }
        let o = |dry_run| Opts { claude_dir: claude.clone(), src: src.clone(), dst: dst.clone(), dry_run, no_copy_files: false };

        run(&o(true)).unwrap();
        assert!(!dst.exists());

        run(&o(false)).unwrap();
        assert!(dst.join("f.txt").exists());
        let new_sub = claude.join("projects").join(encode_path(&dst.join("sub"))).join("s.jsonl");
        assert_eq!(cwd_of(&new_sub), dst.join("sub").display().to_string());
        let old = claude.join("projects").join(encode_path(&src)).join("s.jsonl");
        assert_eq!(cwd_of(&old), src.display().to_string());
        assert!(run(&o(false)).is_err(), "second copy must refuse to overwrite");
    }
}
