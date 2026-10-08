//! `claude-sessions tui`: browse projects and sessions, tick some, and delete, move,
//! copy or export them. State and key handling live in `app` (no terminal needed),
//! drawing in `ui`; this file is the event loop and the code that acts on an `Effect`.

mod app;
mod ui;

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::mv::{conflicts, running_claudes};
use crate::{cp, export, mv, rm, stats};
use app::{App, Effect, ProjectRow};

fn find<'a>(app: &'a App, dir: &Path) -> Option<&'a ProjectRow> {
    app.rows.iter().find(|r| r.dir == dir)
}

fn rm_opts(claude_dir: &Path, purge_config: bool) -> rm::Opts {
    rm::Opts { claude_dir: claude_dir.to_path_buf(), target: None, older_than: None, dry_run: false, yes: true, force: false, purge_config, interactive: false }
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

/// Runs a printing command (mv / cp) with the normal terminal, then comes back.
fn outside(terminal: &mut DefaultTerminal, f: impl FnOnce() -> Result<()>) -> Result<String> {
    ratatui::restore();
    println!();
    let res = f();
    let msg = match &res {
        Ok(()) => "Done".to_string(),
        Err(e) => {
            println!("error: {e:#}");
            format!("Failed: {e:#}")
        }
    };
    print!("\nPress Enter to return to the list ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    *terminal = ratatui::init();
    Ok(msg)
}

fn perform(terminal: &mut DefaultTerminal, claude_dir: &Path, app: &mut App, effect: Effect) -> Result<bool> {
    let status = match effect {
        Effect::Quit => return Ok(true),
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
        Effect::Move { src, dst } => outside(terminal, || mv::run(&mv::Opts { claude_dir: claude_dir.to_path_buf(), src, dst: dst.into(), dry_run: false, no_move_files: false, force: false }))?,
        Effect::Copy { src, dst } => outside(terminal, || cp::run(&cp::Opts { claude_dir: claude_dir.to_path_buf(), src, dst: dst.into(), dry_run: false, no_copy_files: false }))?,
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
    app.reload(app::load(claude_dir)?);
    app.status = status;
    Ok(false)
}

pub fn run(claude_dir: &Path) -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("the TUI needs an interactive terminal");
    }
    let mut app = App::new(app::load(claude_dir).context("cannot load sessions")?);
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| ui::draw(f, &app))?;
            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(effect) = app.handle_key(key) {
                if perform(&mut terminal, claude_dir, &mut app, effect)? {
                    return Ok(());
                }
            }
        }
    })();
    ratatui::restore();
    result
}
