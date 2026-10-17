//! `claude-sessions tui`: browse projects and sessions, tick some, and delete, move,
//! copy or export them. State and key handling live in `app` (no terminal needed),
//! drawing in `ui`; this file is the event loop and the code that acts on an `Effect`.

mod app;
mod ui;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::mv::{conflicts, running_claudes};
use crate::{cp, export, memory, mv, rm, scan, stats};
use app::{App, Effect, Mode, ProjectRow};

fn find<'a>(app: &'a App, dir: &Path) -> Option<&'a ProjectRow> {
    app.rows.iter().find(|r| r.dir == dir)
}

fn rm_opts(claude_dir: &Path, purge_config: bool) -> rm::Opts {
    rm::Opts { claude_dir: claude_dir.to_path_buf(), target: None, older_than: None, dry_run: false, yes: true, force: false, purge_config, interactive: false, keep_memory: false }
}

/// Deletes `items` unless Claude Code is running where it would matter. Returns the status line.
fn delete(claude_dir: &Path, purge_config: bool, items: Vec<rm::Item>) -> String {
    let running = running_claudes(claude_dir);
    let cwds: Vec<&Path> = items.iter().map(|i| i.cwd.as_path()).collect();
    if let Some(r) = conflicts(&running, &cwds).first() {
        return format!("Refused: Claude Code (pid {}) is running in {}; exit it first", r.pid, r.cwd.display());
    }
    if purge_config && !running.is_empty() {
        return format!("Refused: purging config while Claude Code runs ({} process(es)); exit it or turn the option off", running.len());
    }
    match rm::execute(&rm_opts(claude_dir, purge_config), &items) {
        Ok(d) => format!("Deleted {} session(s) in {} folder(s){}", d.sessions, d.folders, if d.edits.is_empty() { "" } else { "; config updated, backups kept" }),
        Err(e) => format!("Delete failed: {e:#}"),
    }
}

/// Runs `$SHELL` (`%COMSPEC%` on Windows) in `dir` and waits for it to exit.
fn spawn_shell(dir: &Path) -> Result<()> {
    let shell = std::env::var_os(if cfg!(windows) { "COMSPEC" } else { "SHELL" })
        .unwrap_or_else(|| if cfg!(windows) { "cmd".into() } else { "/bin/sh".into() });
    Command::new(&shell)
        .current_dir(dir)
        .env("CLAUDE_SESSIONS_TUI", "1")
        .status()
        .with_context(|| format!("cannot start {}", Path::new(&shell).display()))?;
    Ok(())
}

/// Runs `claude` in `dir` and waits for it to exit. Through `cmd /C` on Windows, where
/// `claude` may be a `.cmd` shim that `Command` cannot start directly.
fn spawn_claude(dir: &Path) -> Result<()> {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "claude"]);
        c
    } else {
        Command::new("claude")
    };
    cmd.current_dir(dir)
        .env("CLAUDE_SESSIONS_TUI", "1")
        .status()
        .context("cannot start claude (is it on PATH?)")?;
    Ok(())
}

/// Runs a command with the normal terminal, then comes back. `pause`: wait for Enter first
/// (for commands whose output should be read).
fn outside(terminal: &mut DefaultTerminal, f: impl FnOnce() -> Result<()>, pause: bool) -> Result<String> {
    ratatui::restore();
    if pause {
        println!();
    }
    let res = f();
    let msg = match &res {
        Ok(()) => "Done".to_string(),
        Err(e) => {
            println!("error: {e:#}");
            format!("Failed: {e:#}")
        }
    };
    if pause || res.is_err() {
        print!("\nPress Enter to return to the list ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
    }
    *terminal = ratatui::init();
    Ok(msg)
}

fn perform(terminal: &mut DefaultTerminal, claude_dir: &Path, base: Option<&Path>, app: &mut App, effect: Effect, chosen: &mut Option<PathBuf>) -> Result<bool> {
    let status = match effect {
        Effect::Quit => return Ok(true),
        Effect::Cd(dir) => {
            *chosen = Some(dir);
            return Ok(true);
        }
        Effect::Shell(dir) => {
            let msg = outside(terminal, || spawn_shell(&dir), false)?;
            if msg == "Done" { format!("Back from the shell in {}", dir.display()) } else { msg }
        }
        Effect::Claude(dir) => {
            let msg = outside(terminal, || spawn_claude(&dir), false)?;
            if msg == "Done" { format!("Back from Claude in {}", dir.display()) } else { msg }
        }
        Effect::Reload => "Reloaded".to_string(),
        Effect::DeleteProjects(dirs) => {
            let items = dirs.iter().filter_map(|d| find(app, d)).map(|r| rm::project_item(claude_dir, &r.dir, &r.cwd)).collect();
            delete(claude_dir, app.purge_config, items)
        }
        Effect::DeleteSessions { dir, ids } => match find(app, &dir) {
            Some(r) => {
                let files = ids.iter().map(|id| dir.join(format!("{id}.jsonl"))).filter(|f| f.is_file()).collect();
                let item = rm::sessions_item(claude_dir, &r.dir, &r.cwd, files);
                delete(claude_dir, app.purge_config, vec![item])
            }
            None => "Project not found".into(),
        },
        Effect::Move { src, dst } => outside(
            terminal,
            || {
                let no_move_files = mv::effective_sessions_only(&src, false);
                mv::run(&mv::Opts { claude_dir: claude_dir.to_path_buf(), src, dst: dst.into(), dry_run: false, no_move_files, force: false })
            },
            true,
        )?,
        Effect::Copy { src, dst } => outside(terminal, || cp::run(&cp::Opts { claude_dir: claude_dir.to_path_buf(), src, dst: dst.into(), dry_run: false, no_copy_files: false }), true)?,
        Effect::EditMemory(path) => outside(terminal, || memory::open_in_editor(&path), false)?,
        Effect::DeleteMemory { dir, files } => match find(app, &dir) {
            Some(r) => {
                let project = scan::Project { dir: r.dir.clone(), cwd: r.cwd.clone() };
                let picked: Vec<memory::MemoryFile> = r.memory.iter().filter(|m| files.contains(&m.file)).cloned().collect();
                match memory::check_running(claude_dir, &project).and_then(|_| memory::remove(&r.dir, &picked)) {
                    Ok(d) => format!("Deleted {} memory file(s), {} MEMORY.md line(s){}", d.files, d.index_lines, if d.backup.is_some() { "; backup kept" } else { "" }),
                    Err(e) => format!("Refused: {e:#}"),
                }
            }
            None => "Project not found".into(),
        },
        Effect::ExportMemory { dir, path } => match find(app, &dir) {
            Some(r) => match std::fs::write(&path, memory::to_markdown(&r.cwd, &r.memory)) {
                Ok(()) => format!("Exported memory to {path}"),
                Err(e) => format!("Export failed: {e}"),
            },
            None => "Project not found".into(),
        },
        Effect::Export { dir, id, path } => match find(app, &dir) {
            Some(r) => {
                let file = dir.join(format!("{id}.jsonl"));
                let md = export::to_markdown(&stats::analyze_session(&file), &r.cwd, &export::load_turns(&file));
                match std::fs::write(&path, md) {
                    Ok(()) => format!("Exported to {path}"),
                    Err(e) => format!("Export failed: {e}"),
                }
            }
            None => "Project not found".into(),
        },
    };
    app.reload(app::load(claude_dir, base)?);
    if status.starts_with("Refused") || status.starts_with("Delete failed") {
        app.mode = Mode::Notice(status.clone());
    }
    app.status = status;
    Ok(false)
}

/// Runs the TUI. If the user chose "quit and cd here", the directory is written to `cd_file`
/// (for a shell wrapper) or printed to stdout.
pub fn run(claude_dir: &Path, path: Option<&Path>, cd_file: Option<&Path>) -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("the TUI needs an interactive terminal");
    }
    let base = path.map(crate::scan::resolve).transpose()?;
    let mut loader = app::Loader::default();
    let mut app = App::new(loader.load(claude_dir, base.as_deref()).context("cannot load sessions")?);
    let (request_tx, request_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let refresh_dir = claude_dir.to_path_buf();
    let refresh_base = base.clone();
    // File parsing never blocks terminal input. At most one scan is in flight.
    std::thread::spawn(move || {
        while request_rx.recv().is_ok() {
            let rows = loader.load(&refresh_dir, refresh_base.as_deref());
            if result_tx.send(rows).is_err() {
                break;
            }
        }
    });
    let mut next_refresh = Instant::now() + Duration::from_secs(1);
    let mut refreshing = false;
    let mut discard_refresh = false;
    let mut terminal = ratatui::init();
    let mut chosen = None;
    let result = (|| -> Result<()> {
        loop {
            // Keep modal targets stable until the user finishes the operation.
            if matches!(app.mode, Mode::Normal | Mode::Filter | Mode::Help) {
                if let Ok(result) = result_rx.try_recv() {
                    refreshing = false;
                    next_refresh = Instant::now() + Duration::from_secs(1);
                    if !discard_refresh {
                        match result {
                            Ok(rows) => app.refresh(rows),
                            Err(e) => app.status = format!("Auto-refresh failed: {e:#}"),
                        }
                    }
                    discard_refresh = false;
                }
                if !refreshing && Instant::now() >= next_refresh {
                    refreshing = request_tx.send(()).is_ok();
                    next_refresh = Instant::now() + Duration::from_secs(1);
                }
            }
            terminal.draw(|f| ui::draw(f, &app))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(effect) = app.handle_key(key) {
                if perform(&mut terminal, claude_dir, base.as_deref(), &mut app, effect, &mut chosen)? {
                    return Ok(());
                }
                // Discard a snapshot started before a management operation.
                discard_refresh = refreshing;
                next_refresh = Instant::now() + Duration::from_secs(1);
            }
        }
    })();
    ratatui::restore();
    result?;
    if let Some(dir) = chosen {
        match cd_file {
            Some(f) => std::fs::write(f, dir.display().to_string()).with_context(|| format!("cannot write {}", f.display()))?,
            None => println!("{}", dir.display()),
        }
    }
    Ok(())
}
