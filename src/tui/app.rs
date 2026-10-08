//! Terminal-free state of the TUI: what is listed, where the cursor is, what is ticked,
//! and what a key press asks the outside world to do. Everything here is unit-testable.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::export::{Turn, load_turns};
use crate::scan::{list_projects, session_files};
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
}

impl ProjectRow {
    pub fn key(&self) -> String {
        self.dir.display().to_string()
    }
}

pub fn load(claude_dir: &Path) -> Result<Vec<ProjectRow>> {
    let mut rows = Vec::new();
    for p in list_projects(claude_dir)? {
        let mut sessions: Vec<SessionStat> = session_files(&p.dir).iter().map(|f| analyze_session(f)).collect();
        sessions.sort_by(|a, b| b.last.cmp(&a.last));
        rows.push(ProjectRow {
            orphan: !p.cwd.exists(),
            bytes: sessions.iter().map(|s| s.bytes).sum(),
            messages: sessions.iter().map(|s| s.messages).sum(),
            last: sessions.iter().filter_map(|s| s.last.clone()).max(),
            sessions,
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    Move(PathBuf),
    Copy(PathBuf),
    Export { dir: PathBuf, id: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Normal,
    /// Typing the filter text.
    Filter,
    Confirm { question: String, effect: Effect },
    Input { label: String, text: String, kind: InputKind },
    Help,
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
        if self.view == View::Sessions && self.proj >= self.rows.len() {
            self.view = View::Projects;
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

    pub fn len(&self) -> usize {
        match self.view {
            View::Projects => self.visible_projects().len(),
            View::Sessions => self.visible_sessions().len(),
            View::Session => 0,
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
            View::Session => None,
        }
    }

    /// What a delete would act on: the ticked rows, or the row under the cursor.
    fn targets(&self) -> Vec<String> {
        let ticked: Vec<String> = match self.view {
            View::Projects => self.visible_projects().iter().map(|i| self.rows[*i].key()).collect::<Vec<_>>(),
            View::Sessions => self.visible_sessions().iter().map(|i| self.rows[self.proj].sessions[*i].id.clone()).collect(),
            View::Session => vec![],
        }
        .into_iter()
        .filter(|k| self.selected.contains(k))
        .collect();
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
            View::Session => {}
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
                KeyCode::Char('p') => {
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
        }
    }

    /// Enter / →: drill one level down.
    fn open(&mut self) {
        match self.view {
            View::Projects => {
                if let Some(i) = self.visible_projects().get(self.cursor).copied() {
                    self.proj = i;
                    self.view = View::Sessions;
                    self.selected.clear();
                    self.cursor = 0;
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
            View::Session => {}
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
            View::Sessions => {
                self.view = View::Projects;
                self.selected.clear();
                self.cursor = 0;
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
        if self.view == View::Session {
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
                let all: Vec<String> = match self.view {
                    View::Projects => self.visible_projects().iter().map(|i| self.rows[*i].key()).collect(),
                    View::Sessions => self.visible_sessions().iter().map(|i| self.rows[self.proj].sessions[*i].id.clone()).collect(),
                    View::Session => vec![],
                };
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
            KeyCode::Enter | KeyCode::Right => self.open(),
            KeyCode::Char('d') => self.ask_delete(),
            KeyCode::Char('m') => self.ask_path(true),
            KeyCode::Char('c') => self.ask_path(false),
            KeyCode::Char('e') => self.ask_export(),
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
        code(&mut a, KeyCode::Enter);
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

    #[test]
    fn reload_drops_ticks_and_keeps_cursor_valid() {
        let mut a = app();
        press(&mut a, "G ");
        a.reload(vec![row("/only", false, vec![])]);
        assert!(a.selected.is_empty());
        assert_eq!(a.cursor, 0);
    }
}
