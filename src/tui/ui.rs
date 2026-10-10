use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap};

use super::app::{App, Effect, Mode, View};
use crate::export::Turn;

/// Wrap `text` to `width` display columns (CJK counts double), keeping explicit newlines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(2);
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut cur = String::new();
        let mut w = 0;
        for c in raw.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
        out.push(cur);
    }
    out
}

fn session_lines(turns: &[Turn], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for t in turns {
        let color = if t.role == "user" { Color::Cyan } else { Color::Green };
        let mut head = (if t.role == "user" { "User" } else { "Assistant" }).to_string();
        if let Some(ts) = &t.timestamp {
            head.push_str(&format!(" · {}", when(&Some(ts.clone()))));
        }
        if let Some(m) = &t.model {
            head.push_str(&format!(" · {m}"));
        }
        lines.push(Line::styled(head, Style::new().fg(color).add_modifier(Modifier::BOLD)));
        lines.extend(wrap(&t.text, width).into_iter().map(Line::from));
        lines.push(Line::from(""));
    }
    lines
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_048_576 { format!("{:.1} MB", bytes as f64 / 1_048_576.0) } else { format!("{:.0} KB", bytes as f64 / 1024.0) }
}

fn when(t: &Option<String>) -> String {
    t.as_deref().map(|s| s.chars().take(16).collect::<String>().replace('T', " ")).unwrap_or_else(|| "-".into())
}

/// Cuts `s` from the left to at most `w` cells, so its end (the current row) stays visible.
fn tail(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut kept = Vec::new();
    let mut used = 1; // the `…`
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w {
            break;
        }
        kept.push(c);
        used += cw;
    }
    format!("…{}", kept.iter().rev().collect::<String>())
}

/// Page name and its keys for the `?` window; `Enter` menu keys included on the project list.
fn help_lines(app: &App) -> (&'static str, Vec<(&'static str, &'static str)>) {
    let list = [("↑↓ / j k", "move"), ("PgUp PgDn", "move 10 rows"), ("g G / Home End", "first / last"), ("Space", "tick + next"), ("a", "tick all / none"), ("/", "filter (Enter keep, Esc clear)")];
    let read = [("↑↓ / j k", "scroll"), ("PgUp PgDn / b Space", "page"), ("g G / Home End", "top / bottom")];
    let common = [("?", "this help"), ("q / Ctrl-C", "quit")];
    let (name, mut v): (&str, Vec<(&str, &str)>) = match app.view {
        View::Projects => {
            let mut v = list.to_vec();
            v.extend([
                ("Enter", "menu for the directory:"),
                ("  s", "  browse sessions"),
                ("  m", "  browse auto-memory"),
                ("  h", "  shell here (exit returns)"),
                ("  c", "  claude here (exit returns)"),
                ("  x", "  quit and cd here"),
                ("→", "open sessions"),
                ("M", "open auto-memory"),
                ("d", "delete ticked (or cursor) folders; p: purge config"),
                ("m / c", "move / copy the directory + sessions"),
                ("o", "orphans only"),
                ("s", "cycle sort (path, size, last used)"),
                ("r", "reload"),
                ("Esc", "clear filter, then quit"),
            ]);
            ("Projects", v)
        }
        View::Sessions => {
            let mut v = list.to_vec();
            v.extend([
                ("Enter / →", "read the session"),
                ("d", "delete ticked (or cursor) sessions; p: purge config"),
                ("e", "export the session as Markdown"),
                ("M", "this project's auto-memory"),
                ("r", "reload"),
                ("Esc / ←", "back to projects"),
            ]);
            ("Sessions", v)
        }
        View::Memory => {
            let mut v = list.to_vec();
            v.extend([
                ("Enter / →", "read the file"),
                ("e", "edit in $EDITOR"),
                ("d", "delete ticked (or cursor) files + their MEMORY.md lines"),
                ("x", "export all memory as Markdown"),
                ("S", "this project's sessions"),
                ("r", "reload"),
                ("Esc / ←", "back to projects"),
            ]);
            ("Memory", v)
        }
        View::Session => {
            let mut v = read.to_vec();
            v.push(("Esc / ←", "back to the sessions"));
            ("Reading a session", v)
        }
        View::MemoryFile => {
            let mut v = read.to_vec();
            v.extend([("e", "edit in $EDITOR"), ("Esc / ←", "back to the memory list")]);
            ("Reading a memory file", v)
        }
    };
    v.extend(common);
    (name, v)
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());

    let place = match app.view {
        View::Projects | View::Sessions | View::Memory => app.cursor_label().unwrap_or_default(),
        View::Session => app
            .rows
            .get(app.proj)
            .map(|p| {
                let s = app.visible_sessions().get(app.list_cursor()).map(|i| &p.sessions[*i]);
                format!("{} › {} {}", p.cwd.display(), s.map(|s| s.id.chars().take(8).collect::<String>()).unwrap_or_default(), s.and_then(|s| s.title.clone()).unwrap_or_default())
            })
            .unwrap_or_default(),
        View::MemoryFile => {
            let file = app.memo.as_ref().and_then(|(p, _)| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            app.rows.get(app.proj).map(|p| format!("{} › memory › {file}", p.cwd.display())).unwrap_or_default()
        }
    };
    let sort = format!("  sort: {}", app.sort.label());
    // Leave room for the name and sort; the path is cut from the left so the current row stays visible.
    let room = (head.width as usize).saturating_sub(" claude-sessions ".len() + sort.len() + 12);
    let mut title = vec![
        Span::styled(" claude-sessions ", Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        Span::raw(tail(&place, room)),
        Span::styled(sort, Style::new().fg(Color::DarkGray)),
    ];
    if app.only_orphans {
        title.push(Span::styled("  [orphans only]", Style::new().fg(Color::Yellow)));
    }
    if !app.filter.is_empty() || app.mode == Mode::Filter {
        title.push(Span::styled(format!("  /{}", app.filter), Style::new().fg(Color::Green)));
    }
    if !app.selected.is_empty() {
        title.push(Span::styled(format!("  {} ticked", app.selected.len()), Style::new().fg(Color::Magenta)));
    }
    f.render_widget(Paragraph::new(Line::from(title)), head);

    let mut state = TableState::default();
    let hl = Style::new().add_modifier(Modifier::REVERSED);
    match app.view {
        View::Projects => {
            let vis = app.visible_projects();
            let rows: Vec<Row> = vis
                .iter()
                .map(|i| {
                    let r = &app.rows[*i];
                    let mark = if app.is_ticked(&r.key()) { "[x]" } else { "[ ]" };
                    let style = if r.orphan { Style::new().fg(Color::Red) } else { Style::new() };
                    Row::new(vec![
                        mark.to_string(),
                        if r.orphan { "gone".into() } else { String::new() },
                        r.cwd.display().to_string(),
                        r.sessions.len().to_string(),
                        if r.memory.is_empty() { "-".into() } else { r.memory.len().to_string() },
                        r.messages.to_string(),
                        mb(r.bytes),
                        when(&r.last),
                    ])
                    .style(style)
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(4), Constraint::Fill(1), Constraint::Length(8), Constraint::Length(4), Constraint::Length(7), Constraint::Length(9), Constraint::Length(16)];
            let header = Row::new(["", "", "DIRECTORY", "SESSIONS", "MEM", "MSGS", "SIZE", "LAST (UTC)"]).style(Style::new().add_modifier(Modifier::BOLD));
            state.select((!vis.is_empty()).then_some(app.cursor));
            f.render_stateful_widget(Table::new(rows, widths).header(header).row_highlight_style(hl).block(Block::new().borders(Borders::TOP)), body, &mut state);
        }
        View::Sessions => {
            let vis = app.visible_sessions();
            let p = &app.rows[app.proj.min(app.rows.len().saturating_sub(1))];
            let rows: Vec<Row> = vis
                .iter()
                .map(|i| {
                    let s = &p.sessions[*i];
                    Row::new(vec![
                        if app.is_ticked(&s.id) { "[x]".to_string() } else { "[ ]".to_string() },
                        s.id.chars().take(8).collect(),
                        s.title.clone().unwrap_or_else(|| "(untitled)".into()),
                        s.messages.to_string(),
                        mb(s.bytes),
                        when(&s.last),
                    ])
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(8), Constraint::Fill(1), Constraint::Length(6), Constraint::Length(9), Constraint::Length(16)];
            let header = Row::new(["", "ID", "TITLE", "MSGS", "SIZE", "LAST (UTC)"]).style(Style::new().add_modifier(Modifier::BOLD));
            state.select((!vis.is_empty()).then_some(app.cursor));
            f.render_stateful_widget(Table::new(rows, widths).header(header).row_highlight_style(hl).block(Block::new().borders(Borders::TOP)), body, &mut state);
        }
        View::Memory => {
            let vis = app.visible_memory();
            let p = &app.rows[app.proj.min(app.rows.len().saturating_sub(1))];
            let rows: Vec<Row> = vis
                .iter()
                .map(|i| {
                    let m = &p.memory[*i];
                    let kind = if m.is_index() { "index".to_string() } else { m.kind.clone().unwrap_or_default() };
                    Row::new(vec![
                        if app.is_ticked(&m.file) { "[x]".to_string() } else { "[ ]".to_string() },
                        m.file.clone(),
                        kind,
                        m.summary(),
                        format!("{:.1} KB", m.bytes as f64 / 1024.0),
                        when(&m.modified),
                    ])
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(32), Constraint::Length(9), Constraint::Fill(1), Constraint::Length(8), Constraint::Length(16)];
            let header = Row::new(["", "FILE", "TYPE", "DESCRIPTION", "SIZE", "MODIFIED (UTC)"]).style(Style::new().add_modifier(Modifier::BOLD));
            state.select((!vis.is_empty()).then_some(app.cursor));
            f.render_stateful_widget(Table::new(rows, widths).header(header).row_highlight_style(hl).block(Block::new().borders(Borders::TOP)), body, &mut state);
        }
        View::Session | View::MemoryFile => {}
    }
    if matches!(app.view, View::Session | View::MemoryFile) {
        let inner = Rect { y: body.y + 1, height: body.height.saturating_sub(1), ..body };
        let width = inner.width.saturating_sub(1) as usize;
        let lines = match (&app.memo, app.view) {
            (Some((_, text)), View::MemoryFile) => wrap(text, width).into_iter().map(Line::from).collect(),
            _ => session_lines(&app.turns, width),
        };
        let h = inner.height as usize;
        app.view_dims.set((h, lines.len()));
        let scroll = app.scroll.min(lines.len().saturating_sub(h));
        let title = if lines.is_empty() { " (no messages) ".to_string() } else { format!(" {}–{} / {} ", scroll + 1, (scroll + h).min(lines.len()), lines.len()) };
        f.render_widget(Block::new().borders(Borders::TOP).title(title), body);
        f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
    } else if app.len() == 0 {
        f.render_widget(Paragraph::new("  (nothing to show)").style(Style::new().fg(Color::DarkGray)), Rect { y: body.y + 2, height: 1, ..body });
    }

    let hint = match (&app.mode, app.view) {
        (Mode::Filter, _) => " type to filter · Enter keep · Esc clear".to_string(),
        (_, View::Projects) => " ↑↓ move · Space tick · Enter menu · → sessions · M memory · d delete · m move · c copy · / filter · o orphans · s sort · ? help · q quit".to_string(),
        (_, View::Sessions) => " ↑↓ move · Space tick · a all · d delete · e export · M memory · / filter · Enter/→ read · Esc/← back · ? help · q quit".to_string(),
        (_, View::Session) => " ↑↓ scroll · PgUp/PgDn page · g/G top/bottom · Esc/← back · ? help · q quit".to_string(),
        (_, View::Memory) => " ↑↓ move · Space tick · Enter/→ read · e edit · d delete · x export · S sessions · / filter · Esc/← back · ? help · q quit".to_string(),
        (_, View::MemoryFile) => " ↑↓ scroll · PgUp/PgDn page · g/G top/bottom · e edit · Esc/← back · ? help · q quit".to_string(),
    };
    let foot_text = if app.status.is_empty() { Span::styled(hint, Style::new().fg(Color::DarkGray)) } else { Span::styled(format!(" {}", app.status), Style::new().fg(Color::Yellow)) };
    f.render_widget(Paragraph::new(Line::from(foot_text)), foot);

    match &app.mode {
        Mode::Confirm { question, effect } => {
            let area = centered(f.area(), 70, 8);
            f.render_widget(Clear, area);
            let cfg = if app.purge_config { "ON  (history.jsonl lines + .claude.json entry are removed; backed up first)" } else { "off (prompt history and trust settings are kept)" };
            let extra = if matches!(effect, Effect::DeleteMemory { .. }) {
                "Their MEMORY.md lines go too (MEMORY.md is backed up first).".to_string()
            } else {
                format!("[p] also purge config: {cfg}")
            };
            let text = vec![
                Line::from(question.as_str()).style(Style::new().add_modifier(Modifier::BOLD)),
                Line::from(""),
                Line::from(extra),
                Line::from(""),
                Line::from("[y] delete   [n] cancel").style(Style::new().fg(Color::Yellow)),
            ];
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(Block::bordered().title(" Delete ").border_style(Style::new().fg(Color::Red))), area);
        }
        Mode::Input { label, text, .. } => {
            let area = centered(f.area(), 80, 5);
            f.render_widget(Clear, area);
            let lines = vec![Line::from(format!("{text}▏")), Line::from(""), Line::from("Enter confirm · Esc cancel").style(Style::new().fg(Color::DarkGray))];
            f.render_widget(Paragraph::new(lines).block(Block::bordered().title(format!(" {label} "))), area);
        }
        Mode::Menu { row } => {
            let Some(p) = app.rows.get(*row) else { return };
            let area = centered(f.area(), 64, 10);
            f.render_widget(Clear, area);
            let off = if p.orphan { Style::new().fg(Color::DarkGray) } else { Style::new() };
            let mem = if p.memory.is_empty() { "[m] Auto-memory (none)".to_string() } else { format!("[m] Auto-memory ({} file(s)): read, edit, delete", p.memory.len()) };
            let text = vec![
                Line::from("[s] Browse sessions"),
                Line::from(mem).style(if p.memory.is_empty() { Style::new().fg(Color::DarkGray) } else { Style::new() }),
                Line::from("[h] Shell here (exit returns to this list)").style(off),
                Line::from("[c] Claude here (exit returns to this list)").style(off),
                Line::from("[x] Quit and cd here (see README: shell wrapper)").style(off),
                Line::from(if p.orphan { "    directory no longer exists" } else { "" }).style(Style::new().fg(Color::Red)),
                Line::from("Esc cancel").style(Style::new().fg(Color::DarkGray)),
            ];
            f.render_widget(Paragraph::new(text).block(Block::bordered().title(format!(" {} ", p.cwd.display()))), area);
        }
        Mode::Help => {
            let (name, keys) = help_lines(app);
            let kw = keys.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
            let mut lines: Vec<Line> = keys
                .iter()
                .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<kw$}  "), Style::new().fg(Color::Cyan)), Span::raw(*d)]))
                .collect();
            if matches!(app.view, View::Projects | View::Sessions | View::Memory) {
                lines.push(Line::from(""));
                lines.push(Line::from("Deleting is refused while Claude Code runs in that directory.").style(Style::new().fg(Color::DarkGray)));
            }
            let w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 4;
            let area = centered(f.area(), w.max(40), lines.len() as u16 + 2);
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(lines).block(Block::bordered().title(format!(" Help — {name} — any key closes "))), area);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::stats::SessionStat;
    use crate::tui::app::ProjectRow;

    fn app() -> App {
        let s = SessionStat { id: "abcdef123456".into(), title: Some("Fix the bug".into()), messages: 7, bytes: 2_097_152, last: Some("2026-01-02T03:04:05Z".into()), ..Default::default() };
        App::new(vec![ProjectRow {
            dir: "/c/projects/-x".into(),
            cwd: "/x/project".into(),
            orphan: true,
            bytes: s.bytes,
            messages: 7,
            last: s.last.clone(),
            sessions: vec![s],
            memory: vec![],
        }])
    }

    fn render(app: &App) -> String {
        let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn project_list_shows_orphan_and_numbers() {
        let out = render(&app());
        assert!(out.contains("/x/project") && out.contains("gone") && out.contains("2.0 MB") && out.contains("2026-01-02 03:04"), "{out}");
    }

    #[test]
    fn confirm_dialog_shows_purge_switch() {
        let mut a = app();
        a.handle_key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Char('d'), ratatui::crossterm::event::KeyModifiers::NONE));
        let out = render(&a);
        assert!(out.contains("Delete 1 project folder(s)") && out.contains("[p] also purge config") && out.contains("off"), "{out}");
    }

    #[test]
    fn help_window_is_drawn_and_any_key_closes_it() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        a.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let out = render(&a);
        assert!(out.contains("Help — Projects") && out.contains("orphans only") && out.contains("Space"), "{out}");
        assert!(out.contains("shell here") && out.contains("claude here") && out.contains("browse auto-memory") && out.contains("quit and cd here"), "Enter menu keys are listed: {out}");
        a.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!render(&a).contains("Help"));
    }

    #[test]
    fn enter_shows_directory_menu() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let out = render(&a);
        assert!(out.contains("[s] Browse sessions") && out.contains("[h] Shell here") && out.contains("[c] Claude here") && out.contains("[x] Quit and cd") && out.contains("no longer exists"), "{out}");
    }

    #[test]
    fn reading_a_session_shows_turns_with_cjk() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        a.view = View::Session;
        a.turns = vec![
            Turn { role: "user", timestamp: Some("2026-01-02T03:04:05Z".into()), model: None, text: "how are you".into() },
            Turn { role: "assistant", timestamp: None, model: None, text: "你好，我很好".into() },
        ];
        let out = render(&a);
        assert!(out.contains("User · 2026-01-02 03:04") && out.contains("how are you") && out.contains("Assistant") && out.contains('你') && out.contains('很'), "{out}");
        a.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(a.view, View::Sessions);
    }

    #[test]
    fn memory_view_lists_files_and_reads_one() {
        let mut a = app();
        a.rows[0].memory = vec![crate::memory::MemoryFile {
            file: "feedback-x.md".into(),
            path: "/c/projects/-x/memory/feedback-x.md".into(),
            name: Some("x".into()),
            description: Some("prefer filter-repo".into()),
            kind: Some("feedback".into()),
            bytes: 2048,
            modified: Some("2026-01-02T03:04:05Z".into()),
        }];
        a.view = View::Memory;
        let out = render(&a);
        assert!(out.contains("feedback-x.md") && out.contains("feedback") && out.contains("prefer filter-repo") && out.contains("2.0 KB") && out.contains("› memory"), "{out}");
        a.view = View::MemoryFile;
        a.memo = Some(("/c/projects/-x/memory/feedback-x.md".into(), "記住這件事\nline two".into()));
        let out = render(&a);
        assert!(out.contains("memory › feedback-x.md") && out.contains('記') && out.contains("line two") && out.contains("e edit"), "{out}");
    }

    #[test]
    fn help_lists_the_keys_of_the_current_page() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        a.rows[0].memory = vec![crate::memory::MemoryFile {
            file: "MEMORY.md".into(),
            path: "/m".into(),
            name: None,
            description: None,
            kind: None,
            bytes: 1,
            modified: None,
        }];
        let mut help_in = |view: View| {
            a.view = view;
            a.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
            let out = render(&a);
            a.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
            out
        };
        let mem = help_in(View::Memory);
        assert!(mem.contains("Help — Memory") && mem.contains("edit in $EDITOR") && mem.contains("MEMORY.md lines"), "{mem}");
        let ses = help_in(View::Sessions);
        assert!(ses.contains("Help — Sessions") && ses.contains("export the session") && !ses.contains("quit and cd"), "{ses}");
        let read = help_in(View::Session);
        assert!(read.contains("Reading a session") && !read.contains("tick"), "{read}");
    }

    #[test]
    fn title_bar_shows_the_row_under_the_cursor_cut_from_the_left() {
        let mut a = app();
        a.rows[0].cwd = format!("/very/{}/deep/project-name", "long-segment/".repeat(12)).into();
        let top = render(&a).lines().next().unwrap().to_string();
        assert!(top.contains("…") && top.contains("deep/project-name") && top.contains("sort: path"), "{top}");
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("路徑很長的目錄", 7), "…的目錄", "CJK counts two cells");
    }

    #[test]
    fn session_view_lists_title() {
        let mut a = app();
        a.handle_key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Right, ratatui::crossterm::event::KeyModifiers::NONE));
        let out = render(&a);
        assert!(out.contains("Fix the bug") && out.contains("abcdef12"), "{out}");
    }
}
