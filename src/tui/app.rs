//! Terminal-free state of the TUI: what is listed, where the cursor is, what is ticked,
//! and what a key press asks the outside world to do. Everything here is unit-testable.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::export::{Turn, load_turns};
use crate::memory::{self, MemoryFile};
use crate::scan::{list_all_projects, session_files};
use crate::stats::{SessionStat, analyze_session};

pub struct ProjectRow {
    pub dir: PathBuf,
    pub cwd: PathBuf,
    /// The working directory no longer exists.
    pub orphan: bool,
    pub sessions: Vec<SessionStat>,
    pub bytes: u64,
    pub messages: u64,
    pub last: Option<String>,
    /// Auto-memory files, `MEMORY.md` first.
    pub memory: Vec<MemoryFile>,
}

impl ProjectRow {
    pub fn key(&self) -> String {
        self.dir.display().to_string()
    }
}

pub fn load(claude_dir: &Path) -> Result<Vec<ProjectRow>> {
    let mut rows = Vec::new();
    for p in list_all_projects(claude_dir)? {
        let mut sessions: Vec<SessionStat> = session_files(&p.dir).iter().map(|f| analyze_session(f)).collect();
        sessions.sort_by(|a, b| b.last.cmp(&a.last));
        rows.push(ProjectRow {
            orphan: !p.cwd.exists(),
            bytes: sessions.iter().map(|s| s.bytes).sum(),
            messages: sessions.iter().map(|s| s.messages).sum(),
            last: sessions.iter().filter_map(|s| s.last.clone()).max(),
            sessions,
            memory: memory::list(&p.dir),
            dir: p.dir,
            cwd: p.cwd,
        });
    }
    Ok(rows)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    Path,
    Size,
    LastUsed,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Path => "path",
            Sort::Size => "size",
            Sort::LastUsed => "last used",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Projects,
    Sessions,
    /// Reading one session's conversation.
    Session,
    /// The auto-memory files of the drilled project.
    Memory,
    /// Reading one memory file.
    MemoryFile,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Quit,
    Reload,
    /// Delete whole project folders (by `ProjectRow::dir`).
    DeleteProjects(Vec<PathBuf>),
    /// Delete these sessions of one project folder.
    DeleteSessions { dir: PathBuf, ids: Vec<String> },
    Move { src: PathBuf, dst: String },
    Copy { src: PathBuf, dst: String },
    Export { dir: PathBuf, id: String, path: String },
    /// Open a sub-shell in this directory; leaving it returns to the TUI.
    Shell(PathBuf),
    /// Start Claude Code in this directory; leaving it returns to the TUI.
    Claude(PathBuf),
    /// Open this memory file in the editor.
    EditMemory(PathBuf),
    /// Delete these memory files (by file name) of one project folder.
    DeleteMemory { dir: PathBuf, files: Vec<String> },
    /// Write all memory of one project folder to a Markdown file.
    ExportMemory { dir: PathBuf, path: String },
    /// Quit and hand this directory to the caller.
    Cd(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    Move(PathBuf),
    Copy(PathBuf),
    Export { dir: PathBuf, id: String },
    ExportMemory(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Normal,
    /// Typing the filter text.
    Filter,
    Confirm { question: String, effect: Effect },
    Input { label: String, text: String, kind: InputKind },
    Help,
    /// What to do with the directory of project `row` (index into `App::rows`).
    Menu { row: usize },
}

pub struct App {
    pub rows: Vec<ProjectRow>,
    pub view: View,
    pub cursor: usize,
    /// Index into `rows` of the project whose sessions are shown.
    pub proj: usize,
    pub selected: HashSet<String>,
    pub sort: Sort,
    pub filter: String,
    pub only_orphans: bool,
    pub purge_config: bool,
    pub mode: Mode,
    pub status: String,
    /// Conversation of the session being read (`View::Session`).
    pub turns: Vec<Turn>,
    pub scroll: usize,
    /// (viewport height, total lines) of the session text, written by the renderer so
    /// scrolling can be clamped here.
    pub view_dims: Cell<(usize, usize)>,
    /// Cursor of the session list, restored when leaving `View::Session`.
    list_cursor: usize,
    /// The memory file being read (`View::MemoryFile`) and its text.
    pub memo: Option<(PathBuf, String)>,
}

impl App {
    pub fn new(rows: Vec<ProjectRow>) -> App {
        App {
            rows,
            view: View::Projects,
            cursor: 0,
            proj: 0,
            selected: HashSet::new(),
            sort: Sort::Path,
            filter: String::new(),
            only_orphans: false,
            purge_config: false,
            mode: Mode::Normal,
            status: String::new(),
            turns: Vec::new(),
            scroll: 0,
            view_dims: Cell::new((0, 0)),
            list_cursor: 0,
            memo: None,
        }
    }

    /// Swap in freshly loaded rows, keeping the cursor on screen and dropping stale ticks.
    pub fn reload(&mut self, rows: Vec<ProjectRow>) {
        self.rows = rows;
        self.selected.clear();
        if self.view == View::Session {
            self.view = View::Sessions;
            self.turns.clear();
            self.cursor = self.list_cursor;
        }
        if self.view == View::MemoryFile {
            // Stay on the file (it may just have been edited) unless it is gone.
            match self.memo.as_ref().and_then(|(p, _)| std::fs::read_to_string(p).ok().map(|t| (p.clone(), t))) {
                Some(m) => self.memo = Some(m),
                None => {
                    self.view = View::Memory;
                    self.memo = None;
                    self.cursor = self.list_cursor;
                }
            }
        }
        if matches!(self.view, View::Sessions | View::Memory | View::MemoryFile) && self.proj >= self.rows.len() {
            self.view = View::Projects;
            self.memo = None;
        }
        self.clamp();
    }

    fn matches(&self, hay: &str) -> bool {
        self.filter.is_empty() || hay.to_lowercase().contains(&self.filter.to_lowercase())
    }

    /// Indices into `rows`, filtered and sorted.
    pub fn visible_projects(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.rows.len())
            .filter(|i| (!self.only_orphans || self.rows[*i].orphan) && self.matches(&self.rows[*i].cwd.display().to_string()))
            .collect();
        match self.sort {
            Sort::Path => v.sort_by(|a, b| self.rows[*a].cwd.cmp(&self.rows[*b].cwd)),
            Sort::Size => v.sort_by(|a, b| self.rows[*b].bytes.cmp(&self.rows[*a].bytes)),
            Sort::LastUsed => v.sort_by(|a, b| self.rows[*b].last.cmp(&self.rows[*a].last)),
        }
        v
    }

    /// Indices into the drilled project's `sessions`, newest first.
    pub fn visible_sessions(&self) -> Vec<usize> {
        let Some(p) = self.rows.get(self.proj) else { return vec![] };
        let mut v: Vec<usize> = (0..p.sessions.len())
            .filter(|i| {
                let s = &p.sessions[*i];
                self.matches(&format!("{} {}", s.id, s.title.as_deref().unwrap_or("")))
            })
            .collect();
        v.sort_by(|a, b| p.sessions[*b].last.cmp(&p.sessions[*a].last));
        v
    }

    /// Indices into the drilled project's `memory`.
    pub fn visible_memory(&self) -> Vec<usize> {
        let Some(p) = self.rows.get(self.proj) else { return vec![] };
        (0..p.memory.len()).filter(|i| self.matches(&format!("{} {}", p.memory[*i].file, p.memory[*i].summary()))).collect()
    }

    fn memory_under_cursor(&self) -> Option<&MemoryFile> {
        let p = self.rows.get(self.proj)?;
        self.visible_memory().get(self.cursor).map(|i| &p.memory[*i])
    }

    pub fn len(&self) -> usize {
        match self.view {
            View::Projects => self.visible_projects().len(),
            View::Sessions => self.visible_sessions().len(),
            View::Memory => self.visible_memory().len(),
            View::Session | View::MemoryFile => 0,
        }
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.len().saturating_sub(1));
    }

    fn step(&mut self, delta: isize) {
        let max = self.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// Key of the row under the cursor (project dir, or session id).
    fn cursor_key(&self) -> Option<String> {
        match self.view {
            View::Projects => self.visible_projects().get(self.cursor).map(|i| self.rows[*i].key()),
            View::Sessions => self.visible_sessions().get(self.cursor).map(|i| self.rows[self.proj].sessions[*i].id.clone()),
            View::Memory => self.memory_under_cursor().map(|m| m.file.clone()),
            View::Session | View::MemoryFile => None,
        }
    }

    /// Every key of the current list, in display order.
    fn all_keys(&self) -> Vec<String> {
        match self.view {
            View::Projects => self.visible_projects().iter().map(|i| self.rows[*i].key()).collect(),
            View::Sessions => self.visible_sessions().iter().map(|i| self.rows[self.proj].sessions[*i].id.clone()).collect(),
            View::Memory => self.visible_memory().iter().map(|i| self.rows[self.proj].memory[*i].file.clone()).collect(),
            View::Session | View::MemoryFile => vec![],
        }
    }

    /// What a delete would act on: the ticked rows, or the row under the cursor.
    fn targets(&self) -> Vec<String> {
        let ticked: Vec<String> = self.all_keys().into_iter().filter(|k| self.selected.contains(k)).collect();
        if ticked.is_empty() { self.cursor_key().into_iter().collect() } else { ticked }
    }

    pub fn is_ticked(&self, key: &str) -> bool {
        self.selected.contains(key)
    }

    fn mb(b: u64) -> String {
        format!("{:.1} MB", b as f64 / 1_048_576.0)
    }

    fn ask_delete(&mut self) {
        let keys = self.targets();
        if keys.is_empty() {
            return;
        }
        match self.view {
            View::Projects => {
                let rows: Vec<&ProjectRow> = self.rows.iter().filter(|r| keys.contains(&r.key())).collect();
                let (n, b) = (rows.iter().map(|r| r.sessions.len()).sum::<usize>(), rows.iter().map(|r| r.bytes).sum::<u64>());
                self.mode = Mode::Confirm {
                    question: format!("Delete {} project folder(s): {n} session(s), {}?", rows.len(), Self::mb(b)),
                    effect: Effect::DeleteProjects(rows.iter().map(|r| r.dir.clone()).collect()),
                };
            }
            View::Session | View::MemoryFile => {}
            View::Memory => {
                let Some(p) = self.rows.get(self.proj) else { return };
                let index = if keys.iter().any(|k| k == memory::INDEX) { " (incl. the MEMORY.md index)" } else { "" };
                self.mode = Mode::Confirm {
                    question: format!("Delete {} memory file(s){index} of {}?", keys.len(), p.cwd.display()),
                    effect: Effect::DeleteMemory { dir: p.dir.clone(), files: keys },
                };
            }
            View::Sessions => {
                let Some(p) = self.rows.get(self.proj) else { return };
                let b: u64 = p.sessions.iter().filter(|s| keys.contains(&s.id)).map(|s| s.bytes).sum();
                self.mode = Mode::Confirm {
                    question: format!("Delete {} session(s) of {}, {}?", keys.len(), p.cwd.display(), Self::mb(b)),
                    effect: Effect::DeleteSessions { dir: p.dir.clone(), ids: keys },
                };
            }
        }
    }

    fn ask_path(&mut self, move_it: bool) {
        if self.view != View::Projects {
            return;
        }
        let Some(i) = self.visible_projects().get(self.cursor).copied() else { return };
        let cwd = self.rows[i].cwd.clone();
        let (label, kind) = if move_it {
            ("Move to", InputKind::Move(cwd.clone()))
        } else {
            ("Copy to", InputKind::Copy(cwd.clone()))
        };
        self.mode = Mode::Input { label: format!("{label} (from {})", cwd.display()), text: cwd.display().to_string(), kind };
    }

    fn ask_export(&mut self) {
        if self.view == View::Memory {
            let Some(p) = self.rows.get(self.proj) else { return };
            let name = p.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
            self.mode = Mode::Input { label: "Export memory to".into(), text: format!("{name}-memory.md"), kind: InputKind::ExportMemory(p.dir.clone()) };
            return;
        }
        if self.view != View::Sessions {
            return;
        }
        let Some(p) = self.rows.get(self.proj) else { return };
        let Some(i) = self.visible_sessions().get(self.cursor).copied() else { return };
        let id = p.sessions[i].id.clone();
        self.mode = Mode::Input {
            label: "Export to".into(),
            text: format!("{id}.md"),
            kind: InputKind::Export { dir: p.dir.clone(), id },
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Effect> {
        match self.mode.clone() {
            Mode::Normal => self.key_normal(key),
            Mode::Filter => {
                match key.code {
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Backspace => {
                        self.filter.pop();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => self.filter.push(c),
                    _ => {}
                }
                self.clamp();
                None
            }
            Mode::Confirm { effect, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.mode = Mode::Normal;
                    Some(effect)
                }
                KeyCode::Char('p') if !matches!(effect, Effect::DeleteMemory { .. }) => {
                    self.purge_config = !self.purge_config;
                    None
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.mode = Mode::Normal;
                    None
                }
                _ => None,
            },
            Mode::Input { label, mut text, kind } => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        self.mode = Mode::Normal;
                        let t = text.trim().to_string();
                        if t.is_empty() {
                            return None;
                        }
                        return Some(match kind {
                            InputKind::Move(src) => Effect::Move { src, dst: t },
                            InputKind::Copy(src) => Effect::Copy { src, dst: t },
                            InputKind::Export { dir, id } => Effect::Export { dir, id, path: t },
                            InputKind::ExportMemory(dir) => Effect::ExportMemory { dir, path: t },
                        });
                    }
                    KeyCode::Backspace => {
                        text.pop();
                        self.mode = Mode::Input { label, text, kind };
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.push(c);
                        self.mode = Mode::Input { label, text, kind };
                    }
                    _ => {}
                }
                None
            }
            Mode::Help => {
                self.mode = Mode::Normal;
                None
            }
            Mode::Menu { row } => self.key_menu(row, key),
        }
    }

    fn key_menu(&mut self, row: usize, key: KeyEvent) -> Option<Effect> {
        let Some(p) = self.rows.get(row) else {
            self.mode = Mode::Normal;
            return None;
        };
        let cwd = p.cwd.clone();
        let orphan = p.orphan;
        match key.code {
            KeyCode::Char('s') | KeyCode::Enter | KeyCode::Right => {
                self.mode = Mode::Normal;
                self.open_project(row);
            }
            KeyCode::Char('m') => {
                self.mode = Mode::Normal;
                self.open_memory(row);
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('h') | KeyCode::Char('c') | KeyCode::Char('x') if orphan => {
                self.mode = Mode::Normal;
                self.status = format!("{} no longer exists", cwd.display());
            }
            KeyCode::Char('h') => {
                self.mode = Mode::Normal;
                return Some(Effect::Shell(cwd));
            }
            KeyCode::Char('c') => {
                self.mode = Mode::Normal;
                return Some(Effect::Claude(cwd));
            }
            KeyCode::Char('x') => {
                self.mode = Mode::Normal;
                return Some(Effect::Cd(cwd));
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
        None
    }

    fn open_project(&mut self, row: usize) {
        self.proj = row;
        self.view = View::Sessions;
        self.selected.clear();
        self.cursor = 0;
    }

    /// Shows the memory of project `row`, or says there is none.
    fn open_memory(&mut self, row: usize) {
        let Some(p) = self.rows.get(row) else { return };
        if p.memory.is_empty() {
            self.status = format!("{} has no auto-memory", p.cwd.display());
            return;
        }
        self.open_project(row);
        self.view = View::Memory;
    }

    /// Where the cursor is, for the title bar: `n/total` and the full path (or session /
    /// memory file) of the row under it. `None` in the reading views.
    pub fn cursor_label(&self) -> Option<String> {
        let pos = |n: usize| format!("{}/{n}", (self.cursor + 1).min(n));
        match self.view {
            View::Projects => {
                let vis = self.visible_projects();
                Some(match vis.get(self.cursor) {
                    Some(i) => format!("{} › {}", pos(vis.len()), self.rows[*i].cwd.display()),
                    None => "0 project(s)".into(),
                })
            }
            View::Sessions => {
                let p = self.rows.get(self.proj)?;
                let vis = self.visible_sessions();
                let cur = vis.get(self.cursor).map(|i| &p.sessions[*i]);
                Some(match cur {
                    Some(s) => format!("{} › {} {} {}", p.cwd.display(), pos(vis.len()), s.id.chars().take(8).collect::<String>(), s.title.as_deref().unwrap_or("(untitled)")),
                    None => format!("{} › no sessions", p.cwd.display()),
                })
            }
            View::Memory => {
                let p = self.rows.get(self.proj)?;
                let vis = self.visible_memory();
                Some(match self.memory_under_cursor() {
                    Some(m) => format!("{} › memory › {} {}", p.cwd.display(), pos(vis.len()), m.file),
                    None => format!("{} › memory", p.cwd.display()),
                })
            }
            View::Session | View::MemoryFile => None,
        }
    }

    /// Enter / →: drill one level down.
    fn open(&mut self) {
        match self.view {
            View::Projects => {
                if let Some(i) = self.visible_projects().get(self.cursor).copied() {
                    self.open_project(i);
                }
            }
            View::Sessions => {
                let Some(p) = self.rows.get(self.proj) else { return };
                let Some(i) = self.visible_sessions().get(self.cursor).copied() else { return };
                self.turns = load_turns(&p.dir.join(format!("{}.jsonl", p.sessions[i].id)));
                self.list_cursor = self.cursor;
                self.scroll = 0;
                self.view_dims.set((0, 0));
                self.view = View::Session;
            }
            View::Memory => {
                let Some(path) = self.memory_under_cursor().map(|m| m.path.clone()) else { return };
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| format!("(cannot read: {e})"));
                self.memo = Some((path, text));
                self.list_cursor = self.cursor;
                self.scroll = 0;
                self.view_dims.set((0, 0));
                self.view = View::MemoryFile;
            }
            View::Session | View::MemoryFile => {}
        }
    }

    /// Esc / ←: go one level up (in the project list: clear the filter; ← does nothing).
    fn back(&mut self) {
        match self.view {
            View::Session => {
                self.view = View::Sessions;
                self.turns.clear();
                self.cursor = self.list_cursor;
            }
            View::MemoryFile => {
                self.view = View::Memory;
                self.memo = None;
                self.cursor = self.list_cursor;
            }
            View::Sessions | View::Memory => {
                self.view = View::Projects;
                self.selected.clear();
                // Back onto the project we came from (it may have moved after a sort or reload).
                self.cursor = self.visible_projects().iter().position(|i| *i == self.proj).unwrap_or(0);
            }
            View::Projects => self.filter.clear(),
        }
    }

    pub fn list_cursor(&self) -> usize {
        self.list_cursor
    }

    fn max_scroll(&self) -> usize {
        let (h, total) = self.view_dims.get();
        total.saturating_sub(h)
    }

    fn key_session(&mut self, key: KeyEvent) -> Option<Effect> {
        let page = self.view_dims.get().0.saturating_sub(1).max(1) as isize;
        let delta = match key.code {
            KeyCode::Char('q') => return Some(Effect::Quit),
            KeyCode::Esc | KeyCode::Left => {
                self.back();
                return None;
            }
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::PageDown | KeyCode::Char(' ') => page,
            KeyCode::PageUp | KeyCode::Char('b') => -page,
            KeyCode::Home | KeyCode::Char('g') => isize::MIN / 2,
            KeyCode::End | KeyCode::Char('G') => isize::MAX / 2,
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                return None;
            }
            KeyCode::Char('e') if self.view == View::MemoryFile => return self.memo.as_ref().map(|(p, _)| Effect::EditMemory(p.clone())),
            _ => return None,
        };
        self.scroll = (self.scroll as isize).saturating_add(delta).clamp(0, self.max_scroll() as isize) as usize;
        None
    }

    fn key_normal(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Effect::Quit);
        }
        self.status.clear();
        if matches!(self.view, View::Session | View::MemoryFile) {
            return self.key_session(key);
        }
        match key.code {
            KeyCode::Char('q') => return Some(Effect::Quit),
            KeyCode::Esc => {
                if self.view == View::Projects && self.filter.is_empty() {
                    return Some(Effect::Quit);
                }
                self.back();
            }
            KeyCode::Left if self.view != View::Projects => self.back(),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::PageDown => self.step(10),
            KeyCode::PageUp => self.step(-10),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = self.len().saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(k) = self.cursor_key() {
                    if !self.selected.remove(&k) {
                        self.selected.insert(k);
                    }
                    self.step(1);
                }
            }
            KeyCode::Char('a') => {
                let all = self.all_keys();
                if all.iter().all(|k| self.selected.contains(k)) {
                    self.selected.clear();
                } else {
                    self.selected.extend(all);
                }
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('o') => {
                self.only_orphans = !self.only_orphans;
                self.clamp();
            }
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    Sort::Path => Sort::Size,
                    Sort::Size => Sort::LastUsed,
                    Sort::LastUsed => Sort::Path,
                };
            }
            KeyCode::Enter if self.view == View::Projects => {
                if let Some(i) = self.visible_projects().get(self.cursor).copied() {
                    self.mode = Mode::Menu { row: i };
                }
            }
            KeyCode::Enter | KeyCode::Right => self.open(),
            KeyCode::Char('d') => self.ask_delete(),
            KeyCode::Char('m') => self.ask_path(true),
            KeyCode::Char('c') => self.ask_path(false),
            KeyCode::Char('M') if self.view == View::Projects => {
                if let Some(i) = self.visible_projects().get(self.cursor).copied() {
                    self.open_memory(i);
                }
            }
            KeyCode::Char('M') if self.view == View::Sessions => self.open_memory(self.proj),
            KeyCode::Char('S') if self.view == View::Memory => self.open_project(self.proj),
            KeyCode::Char('e') if self.view == View::Memory => {
                return self.memory_under_cursor().map(|m| Effect::EditMemory(m.path.clone()));
            }
            KeyCode::Char('e') => self.ask_export(),
            KeyCode::Char('x') if self.view == View::Memory => self.ask_export(),
            KeyCode::Char('r') => return Some(Effect::Reload),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, last: &str, bytes: u64) -> SessionStat {
        SessionStat { id: id.into(), title: Some(format!("title {id}")), last: Some(last.into()), bytes, messages: 2, ..Default::default() }
    }

    fn row(cwd: &str, orphan: bool, sessions: Vec<SessionStat>) -> ProjectRow {
        ProjectRow {
            dir: PathBuf::from(format!("/claude/projects/{}", cwd.replace('/', "-"))),
            cwd: PathBuf::from(cwd),
            orphan,
            bytes: sessions.iter().map(|s| s.bytes).sum(),
            messages: 0,
            last: sessions.iter().filter_map(|s| s.last.clone()).max(),
            sessions,
            memory: vec![],
        }
    }

    fn app() -> App {
        App::new(vec![
            row("/b/beta", false, vec![sess("b1", "2026-02-01", 10), sess("b2", "2026-03-01", 20)]),
            row("/a/alpha", true, vec![sess("a1", "2026-01-01", 500)]),
            row("/c/gamma", false, vec![sess("c1", "2026-04-01", 5)]),
        ])
    }

    fn press(a: &mut App, keys: &str) -> Vec<Effect> {
        keys.chars().filter_map(|c| a.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))).collect()
    }

    fn code(a: &mut App, c: KeyCode) -> Option<Effect> {
        a.handle_key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    fn cwds(a: &App) -> Vec<String> {
        a.visible_projects().iter().map(|i| a.rows[*i].cwd.display().to_string()).collect()
    }

    #[test]
    fn enter_in_projects_opens_menu_with_shell_cd_and_sessions() {
        let mut a = app(); // sorted by path: alpha (orphan), beta, gamma
        code(&mut a, KeyCode::Down); // beta
        assert_eq!(code(&mut a, KeyCode::Enter), None);
        assert!(matches!(a.mode, Mode::Menu { .. }));
        assert_eq!(press(&mut a, "h"), vec![Effect::Shell(PathBuf::from("/b/beta"))]);
        assert_eq!(a.mode, Mode::Normal);
        code(&mut a, KeyCode::Enter);
        assert_eq!(press(&mut a, "c"), vec![Effect::Claude(PathBuf::from("/b/beta"))]);
        assert_eq!(a.mode, Mode::Normal);
        code(&mut a, KeyCode::Enter);
        assert_eq!(press(&mut a, "x"), vec![Effect::Cd(PathBuf::from("/b/beta"))]);
        code(&mut a, KeyCode::Enter);
        press(&mut a, "q"); // closes the menu, does not quit
        assert_eq!((a.mode.clone(), a.view), (Mode::Normal, View::Projects));
        code(&mut a, KeyCode::Enter);
        press(&mut a, "s");
        assert_eq!(a.view, View::Sessions);
        assert_eq!(a.rows[a.proj].cwd, PathBuf::from("/b/beta"));
    }

    #[test]
    fn menu_refuses_shell_and_cd_for_missing_directory() {
        let mut a = app();
        code(&mut a, KeyCode::Enter); // alpha is the orphan
        assert!(press(&mut a, "h").is_empty());
        assert!(a.status.contains("no longer exists"));
        code(&mut a, KeyCode::Enter);
        assert!(press(&mut a, "c").is_empty());
        code(&mut a, KeyCode::Enter);
        assert!(press(&mut a, "x").is_empty());
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut a = app();
        press(&mut a, "kk");
        assert_eq!(a.cursor, 0);
        press(&mut a, "jjjjj");
        assert_eq!(a.cursor, 2);
        press(&mut a, "g");
        assert_eq!(a.cursor, 0);
        press(&mut a, "G");
        assert_eq!(a.cursor, 2);
    }

    #[test]
    fn sorts_cycle_and_order() {
        let mut a = app();
        assert_eq!(cwds(&a), ["/a/alpha", "/b/beta", "/c/gamma"]);
        press(&mut a, "s");
        assert_eq!(a.sort, Sort::Size);
        assert_eq!(cwds(&a), ["/a/alpha", "/b/beta", "/c/gamma"], "alpha 500 > beta 30 > gamma 5");
        press(&mut a, "s");
        assert_eq!(cwds(&a), ["/c/gamma", "/b/beta", "/a/alpha"], "newest first");
        press(&mut a, "s");
        assert_eq!(a.sort, Sort::Path);
    }

    #[test]
    fn filter_and_orphan_view() {
        let mut a = app();
        press(&mut a, "/");
        assert_eq!(a.mode, Mode::Filter);
        press(&mut a, "BET");
        code(&mut a, KeyCode::Enter);
        assert_eq!(cwds(&a), ["/b/beta"]);
        code(&mut a, KeyCode::Esc);
        assert_eq!(cwds(&a).len(), 3, "esc clears the filter first");
        press(&mut a, "o");
        assert_eq!(cwds(&a), ["/a/alpha"]);
        press(&mut a, "o");
        assert_eq!(cwds(&a).len(), 3);
    }

    #[test]
    fn filter_clamps_cursor() {
        let mut a = app();
        press(&mut a, "G/alpha");
        code(&mut a, KeyCode::Enter);
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn tick_then_delete_asks_and_returns_effect() {
        let mut a = app();
        press(&mut a, " ");
        press(&mut a, " ");
        assert_eq!(a.selected.len(), 2);
        assert_eq!(a.cursor, 2);
        assert!(press(&mut a, "d").is_empty());
        let Mode::Confirm { question, .. } = &a.mode else { panic!("expected confirm") };
        assert!(question.contains("2 project folder(s)") && question.contains("3 session(s)"), "{question}");
        let eff = press(&mut a, "y");
        let [Effect::DeleteProjects(dirs)] = eff.as_slice() else { panic!("{eff:?}") };
        assert_eq!(dirs.len(), 2);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn delete_without_ticks_uses_cursor_row_and_n_cancels() {
        let mut a = app();
        press(&mut a, "jd");
        let Mode::Confirm { effect: Effect::DeleteProjects(d), .. } = a.mode.clone() else { panic!() };
        assert_eq!(d, vec![PathBuf::from("/claude/projects/-b-beta")]);
        assert!(press(&mut a, "n").is_empty());
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn p_toggles_purge_config_only_inside_confirm() {
        let mut a = app();
        press(&mut a, "p");
        assert!(!a.purge_config);
        press(&mut a, "dp");
        assert!(a.purge_config);
    }

    #[test]
    fn drill_into_sessions_delete_and_export() {
        let mut a = app();
        press(&mut a, "j");
        code(&mut a, KeyCode::Right);
        assert_eq!(a.view, View::Sessions);
        assert_eq!(a.visible_sessions().len(), 2);
        press(&mut a, " ");
        press(&mut a, "d");
        let Mode::Confirm { effect: Effect::DeleteSessions { ids, .. }, .. } = a.mode.clone() else { panic!() };
        assert_eq!(ids, vec!["b2".to_string()], "sessions are newest first");
        press(&mut a, "n");
        press(&mut a, "e");
        let Mode::Input { text, .. } = &a.mode else { panic!() };
        assert_eq!(text, "b1.md");
        code(&mut a, KeyCode::Enter);
        press(&mut a, "m");
        assert_eq!(a.mode, Mode::Normal, "move is a project-level action");
        code(&mut a, KeyCode::Esc);
        assert_eq!(a.view, View::Projects);
    }

    fn session_app() -> (tempfile::TempDir, App) {
        let d = tempfile::tempdir().unwrap();
        let line = |t: &str, text: &str| format!("{{\"type\":\"{t}\",\"message\":{{\"role\":\"{t}\",\"content\":\"{text}\"}}}}\n");
        std::fs::write(d.path().join("s1.jsonl"), line("user", "hello there") + &line("assistant", "你好")).unwrap();
        let mut r = row("/p", false, vec![sess("s1", "2026-01-01", 5), sess("s0", "2025-01-01", 5)]);
        r.dir = d.path().to_path_buf();
        (d, App::new(vec![r]))
    }

    #[test]
    fn arrows_mirror_enter_and_esc_and_session_opens() {
        let (_d, mut a) = session_app();
        code(&mut a, KeyCode::Right);
        assert_eq!(a.view, View::Sessions);
        code(&mut a, KeyCode::Down);
        code(&mut a, KeyCode::Up);
        code(&mut a, KeyCode::Right);
        assert_eq!(a.view, View::Session);
        assert_eq!(a.turns.len(), 2);
        code(&mut a, KeyCode::Left);
        assert_eq!(a.view, View::Sessions);
        code(&mut a, KeyCode::Down);
        code(&mut a, KeyCode::Enter);
        assert!(a.turns.is_empty(), "s0 has no file");
        code(&mut a, KeyCode::Esc);
        assert_eq!((a.view, a.cursor), (View::Sessions, 1), "cursor restored");
        code(&mut a, KeyCode::Left);
        assert_eq!(a.view, View::Projects);
        assert_eq!(code(&mut a, KeyCode::Left), None, "left never quits");
        assert_eq!(a.view, View::Projects);
    }

    #[test]
    fn session_view_scroll_clamps() {
        let (_d, mut a) = session_app();
        code(&mut a, KeyCode::Right);
        code(&mut a, KeyCode::Right);
        a.view_dims.set((5, 12));
        press(&mut a, "G");
        assert_eq!(a.scroll, 7);
        press(&mut a, "j");
        assert_eq!(a.scroll, 7);
        code(&mut a, KeyCode::PageUp);
        assert_eq!(a.scroll, 3);
        press(&mut a, "g");
        assert_eq!(a.scroll, 0);
        press(&mut a, "k");
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn input_edits_text_and_returns_move_effect() {
        let mut a = app();
        press(&mut a, "m");
        for _ in 0.."/a/alpha".len() {
            code(&mut a, KeyCode::Backspace);
        }
        press(&mut a, "/x/y");
        let eff = code(&mut a, KeyCode::Enter);
        assert_eq!(eff, Some(Effect::Move { src: PathBuf::from("/a/alpha"), dst: "/x/y".into() }));
    }

    #[test]
    fn empty_input_and_esc_do_nothing() {
        let mut a = app();
        press(&mut a, "c");
        for _ in 0..20 {
            code(&mut a, KeyCode::Backspace);
        }
        assert_eq!(code(&mut a, KeyCode::Enter), None);
        press(&mut a, "c");
        assert_eq!(code(&mut a, KeyCode::Esc), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn quit_keys() {
        assert_eq!(press(&mut app(), "q"), vec![Effect::Quit]);
        assert_eq!(code(&mut app(), KeyCode::Esc), Some(Effect::Quit));
        let mut a = app();
        assert_eq!(a.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(Effect::Quit));
    }

    fn memory_app() -> (tempfile::TempDir, App) {
        let d = tempfile::tempdir().unwrap();
        let mem = d.path().join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("MEMORY.md"), "- [A](a.md) — first\n").unwrap();
        std::fs::write(mem.join("a.md"), "---\nname: a\ndescription: first\n---\nalpha body\n").unwrap();
        let mut r = row("/p", false, vec![]);
        r.dir = d.path().to_path_buf();
        r.memory = memory::list(&r.dir);
        let mut plain = row("/q", false, vec![sess("q1", "2026-01-01", 1)]);
        plain.dir = d.path().join("none");
        (d, App::new(vec![r, plain]))
    }

    #[test]
    fn menu_m_opens_memory_and_refuses_without_memory() {
        let (_d, mut a) = memory_app();
        code(&mut a, KeyCode::Down); // /q has no memory
        code(&mut a, KeyCode::Enter);
        press(&mut a, "m");
        assert!(a.status.contains("no auto-memory"));
        assert_eq!(a.view, View::Projects);
        code(&mut a, KeyCode::Up);
        code(&mut a, KeyCode::Enter);
        press(&mut a, "m");
        assert_eq!(a.view, View::Memory);
        assert_eq!(a.visible_memory().len(), 2);
        code(&mut a, KeyCode::Left);
        assert_eq!(a.view, View::Projects);
    }

    #[test]
    fn memory_view_reads_edits_deletes_and_exports() {
        let (d, mut a) = memory_app();
        code(&mut a, KeyCode::Enter);
        press(&mut a, "m");
        code(&mut a, KeyCode::Down); // a.md
        assert_eq!(press(&mut a, "e"), vec![Effect::EditMemory(d.path().join("memory/a.md"))]);
        code(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::MemoryFile);
        assert!(a.memo.as_ref().unwrap().1.contains("alpha body"));
        assert_eq!(press(&mut a, "e"), vec![Effect::EditMemory(d.path().join("memory/a.md"))]);
        code(&mut a, KeyCode::Esc);
        assert_eq!((a.view, a.cursor), (View::Memory, 1));
        press(&mut a, "d");
        let Mode::Confirm { effect, question } = a.mode.clone() else { panic!() };
        assert!(!question.contains("index"), "{question}");
        assert_eq!(effect, Effect::DeleteMemory { dir: d.path().to_path_buf(), files: vec!["a.md".into()] });
        press(&mut a, "p");
        assert!(!a.purge_config, "purge config does not apply to memory");
        press(&mut a, "n");
        press(&mut a, "x");
        let Mode::Input { text, .. } = &a.mode else { panic!() };
        assert_eq!(text, "p-memory.md");
        assert_eq!(code(&mut a, KeyCode::Enter), Some(Effect::ExportMemory { dir: d.path().to_path_buf(), path: "p-memory.md".into() }));
    }

    #[test]
    fn reload_keeps_an_open_memory_file_with_fresh_text() {
        let (d, mut a) = memory_app();
        code(&mut a, KeyCode::Enter);
        press(&mut a, "m");
        code(&mut a, KeyCode::Down);
        code(&mut a, KeyCode::Right);
        std::fs::write(d.path().join("memory/a.md"), "edited\n").unwrap();
        let rows = std::mem::take(&mut a.rows);
        a.reload(rows);
        assert_eq!((a.view, a.memo.as_ref().unwrap().1.as_str()), (View::MemoryFile, "edited\n"));
        std::fs::remove_file(d.path().join("memory/a.md")).unwrap();
        let rows = std::mem::take(&mut a.rows);
        a.reload(rows);
        assert_eq!(a.view, View::Memory);
    }

    #[test]
    fn capital_m_opens_memory_from_projects_and_sessions() {
        let (_d, mut a) = memory_app(); // /p has memory, /q has none
        press(&mut a, "jM");
        assert!(a.status.contains("no auto-memory"));
        assert_eq!(a.view, View::Projects);
        press(&mut a, "kM");
        assert_eq!(a.view, View::Memory);
        press(&mut a, "S");
        assert_eq!(a.view, View::Sessions);
        press(&mut a, "M");
        assert_eq!((a.view, a.rows[a.proj].cwd.clone()), (View::Memory, PathBuf::from("/p")));
    }

    #[test]
    fn going_back_returns_to_the_project_we_opened() {
        let mut a = app(); // alpha, beta, gamma
        press(&mut a, "jj");
        code(&mut a, KeyCode::Right);
        code(&mut a, KeyCode::Left);
        assert_eq!(a.cursor, 2);
        code(&mut a, KeyCode::Enter);
        press(&mut a, "s");
        code(&mut a, KeyCode::Esc);
        assert_eq!(cwds(&a)[a.cursor], "/c/gamma");
        code(&mut a, KeyCode::Right);
        press(&mut a, "ss"); // last used: gamma moves to the top
        code(&mut a, KeyCode::Left);
        assert_eq!(cwds(&a)[a.cursor], "/c/gamma", "found again after a re-sort");
    }

    #[test]
    fn cursor_label_names_the_row_under_the_cursor() {
        let mut a = app();
        press(&mut a, "j");
        assert_eq!(a.cursor_label().unwrap(), "2/3 › /b/beta");
        code(&mut a, KeyCode::Right);
        assert_eq!(a.cursor_label().unwrap(), "/b/beta › 1/2 b2 title b2");
        press(&mut a, "/zzz");
        assert_eq!(a.cursor_label().unwrap(), "/b/beta › no sessions");
    }

    #[test]
    fn question_mark_in_the_menu_opens_help() {
        let mut a = app();
        code(&mut a, KeyCode::Enter);
        press(&mut a, "?");
        assert_eq!(a.mode, Mode::Help);
    }

    #[test]
    fn reload_drops_ticks_and_keeps_cursor_valid() {
        let mut a = app();
        press(&mut a, "G ");
        a.reload(vec![row("/only", false, vec![])]);
        assert!(a.selected.is_empty());
        assert_eq!(a.cursor, 0);
    }
}
