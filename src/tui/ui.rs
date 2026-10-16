use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use super::app::{App, Effect, Mode, SortOrder, View};
use crate::export::Turn;

// A shared dark palette keeps numbers, navigation and dialogs consistent.
const BACKGROUND: Color = Color::Rgb(20, 24, 36);
const PANEL: Color = Color::Rgb(28, 34, 49);
const SELECTED: Color = Color::Rgb(47, 62, 86);
const TEXT: Color = Color::Rgb(220, 227, 240);
const MUTED: Color = Color::Rgb(145, 160, 183);
const CYAN: Color = Color::Rgb(110, 211, 226);
const GREEN: Color = Color::Rgb(158, 218, 165);
const YELLOW: Color = Color::Rgb(240, 205, 133);
const PURPLE: Color = Color::Rgb(192, 169, 240);
const RED: Color = Color::Rgb(244, 143, 154);

fn ink(color: Color) -> Style {
    if crate::style::enabled() { Style::new().fg(color) } else { Style::new() }
}

fn surface(fg: Color, bg: Color) -> Style {
    if crate::style::enabled() { ink(fg).bg(bg) } else { Style::new() }
}

fn cell(text: impl Into<String>, color: Color) -> Cell<'static> {
    Cell::from(text.into()).style(ink(color))
}

fn number(text: impl Into<String>, color: Color) -> Cell<'static> {
    Cell::from(Line::from(text.into()).right_aligned()).style(ink(color))
}

fn marker(checked: bool) -> Cell<'static> {
    cell(if checked { "[x]" } else { "[ ]" }, if checked { GREEN } else { MUTED })
}

fn stripe(index: usize) -> Style {
    surface(TEXT, if index % 2 == 0 { BACKGROUND } else { PANEL })
}

fn dialog(title: String, accent: Color) -> Block<'static> {
    Block::bordered().title(title).style(surface(TEXT, PANEL)).border_style(ink(accent)).title_style(ink(accent).bold())
}

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
        let color = if t.role == "user" { CYAN } else { GREEN };
        let mut head = (if t.role == "user" { "User" } else { "Assistant" }).to_string();
        if let Some(ts) = &t.timestamp {
            head.push_str(&format!(" · {}", when(&Some(ts.clone()))));
        }
        if let Some(m) = &t.model {
            head.push_str(&format!(" · {m}"));
        }
        lines.push(Line::styled(head, ink(color).add_modifier(Modifier::BOLD)));
        lines.extend(wrap(&t.text, width).into_iter().map(Line::from));
        lines.push(Line::from(""));
    }
    lines
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_048_576 { format!("{:.1} MB", bytes as f64 / 1_048_576.0) } else { format!("{:.0} KB", bytes as f64 / 1024.0) }
}

fn when(t: &Option<String>) -> String {
    t.as_deref().map(|s| {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|_| s.chars().take(16).collect::<String>().replace('T', " "))
    }).unwrap_or_else(|| "-".into())
}

fn directory_label(path: &std::path::Path) -> String {
    match path.file_name() {
        Some(name) => format!("… {}", name.to_string_lossy()),
        None => path.display().to_string(),
    }
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
                ("s", "sort by column (ascending / descending / original)"),
                ("r", "reload"),
                ("Esc", "clear filter, then quit"),
            ]);
            ("Projects", v)
        }
        View::Sessions => {
            let mut v = list.to_vec();
            v.extend([
                ("Enter / →", "read the session"),
                ("s", "sort by column (ascending / descending / original)"),
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
                ("s", "sort by column (ascending / descending / original)"),
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
    f.render_widget(Block::new().style(surface(TEXT, BACKGROUND)), f.area());
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
    let sort = format!("  sort: {}", app.sort_label());
    // Leave room for the name and sort; the path is cut from the left so the current row stays visible.
    let room = (head.width as usize).saturating_sub(" claude-sessions ".len() + sort.len() + 12);
    let mut title = vec![
        Span::styled(" claude-sessions ", ink(CYAN).bold()),
        Span::raw(tail(&place, room)),
        Span::styled(sort, ink(MUTED)),
    ];
    if app.only_orphans {
        title.push(Span::styled("  [orphans only]", ink(YELLOW)));
    }
    if !app.filter.is_empty() || app.mode == Mode::Filter {
        title.push(Span::styled(format!("  /{}", app.filter), ink(GREEN)));
    }
    if !app.selected.is_empty() {
        title.push(Span::styled(format!("  {} ticked", app.selected.len()), ink(PURPLE)));
    }
    f.render_widget(Paragraph::new(Line::from(title)).style(surface(TEXT, PANEL)), head);

    let mut state = TableState::default();
    let hl = if crate::style::enabled() { Style::new().bg(SELECTED).bold() } else { Style::new().add_modifier(Modifier::REVERSED) };
    match app.view {
        View::Projects => {
            let vis = app.visible_projects();
            let rows: Vec<Row> = vis
                .iter()
                .enumerate()
                .map(|(index, i)| {
                    let r = &app.rows[*i];
                    Row::new(vec![
                        marker(app.is_ticked(&r.key())),
                        cell(if r.orphan { "gone" } else { "" }, RED),
                        cell(directory_label(&r.cwd), if r.orphan { RED } else { TEXT }),
                        number(r.sessions.len().to_string(), CYAN),
                        number(if r.memory.is_empty() { "-".into() } else { r.memory.len().to_string() }, PURPLE),
                        number(r.messages.to_string(), GREEN),
                        number(mb(r.bytes), YELLOW),
                        cell(when(&r.last), MUTED),
                    ])
                    .style(stripe(index))
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(4), Constraint::Fill(1), Constraint::Length(8), Constraint::Length(4), Constraint::Length(7), Constraint::Length(9), Constraint::Length(16)];
            let header = Row::new([Cell::from(""), Cell::from(""), Cell::from("DIRECTORY"), Cell::from(Line::from("SESSIONS").right_aligned()), Cell::from(Line::from("MEM").right_aligned()), Cell::from(Line::from("MSGS").right_aligned()), Cell::from(Line::from("SIZE").right_aligned()), Cell::from("LAST (local)")]).style(surface(CYAN, PANEL).bold());
            state.select((!vis.is_empty()).then_some(app.cursor));
            f.render_stateful_widget(Table::new(rows, widths).header(header).row_highlight_style(hl).block(Block::new().borders(Borders::TOP).border_style(ink(MUTED))), body, &mut state);
        }
        View::Sessions => {
            let vis = app.visible_sessions();
            let p = &app.rows[app.proj.min(app.rows.len().saturating_sub(1))];
            let rows: Vec<Row> = vis
                .iter()
                .enumerate()
                .map(|(index, i)| {
                    let s = &p.sessions[*i];
                    Row::new(vec![
                        marker(app.is_ticked(&s.id)),
                        cell(s.id.chars().take(8).collect::<String>(), PURPLE),
                        cell(s.title.clone().unwrap_or_else(|| "(untitled)".into()), if s.title.is_some() { TEXT } else { MUTED }),
                        number(s.messages.to_string(), GREEN),
                        number(mb(s.bytes), YELLOW),
                        cell(when(&s.last), MUTED),
                    ]).style(stripe(index))
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(8), Constraint::Fill(1), Constraint::Length(6), Constraint::Length(9), Constraint::Length(16)];
            let header = Row::new([Cell::from(""), Cell::from("ID"), Cell::from("TITLE"), Cell::from(Line::from("MSGS").right_aligned()), Cell::from(Line::from("SIZE").right_aligned()), Cell::from("LAST (local)")]).style(surface(CYAN, PANEL).bold());
            state.select((!vis.is_empty()).then_some(app.cursor));
            f.render_stateful_widget(Table::new(rows, widths).header(header).row_highlight_style(hl).block(Block::new().borders(Borders::TOP).border_style(ink(MUTED))), body, &mut state);
        }
        View::Memory => {
            let vis = app.visible_memory();
            let p = &app.rows[app.proj.min(app.rows.len().saturating_sub(1))];
            let rows: Vec<Row> = vis
                .iter()
                .enumerate()
                .map(|(index, i)| {
                    let m = &p.memory[*i];
                    let kind = if m.is_index() { "index".to_string() } else { m.kind.clone().unwrap_or_default() };
                    Row::new(vec![
                        marker(app.is_ticked(&m.file)),
                        cell(m.file.clone(), CYAN),
                        cell(kind, PURPLE),
                        cell(m.summary(), TEXT),
                        number(format!("{:.1} KB", m.bytes as f64 / 1024.0), YELLOW),
                        cell(when(&m.modified), MUTED),
                    ]).style(stripe(index))
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(32), Constraint::Length(9), Constraint::Fill(1), Constraint::Length(8), Constraint::Length(16)];
            let header = Row::new([Cell::from(""), Cell::from("FILE"), Cell::from("TYPE"), Cell::from("DESCRIPTION"), Cell::from(Line::from("SIZE").right_aligned()), Cell::from("MODIFIED (local)")]).style(surface(CYAN, PANEL).bold());
            state.select((!vis.is_empty()).then_some(app.cursor));
            f.render_stateful_widget(Table::new(rows, widths).header(header).row_highlight_style(hl).block(Block::new().borders(Borders::TOP).border_style(ink(MUTED))), body, &mut state);
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
        f.render_widget(Block::new().borders(Borders::TOP).border_style(ink(MUTED)).title(title), body);
        f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
    } else if app.len() == 0 {
        f.render_widget(Paragraph::new("  (nothing to show)").style(ink(MUTED)), Rect { y: body.y + 2, height: 1, ..body });
    }

    let hint = match (&app.mode, app.view) {
        (Mode::Filter, _) => " type to filter · Enter keep · Esc clear".to_string(),
        (_, View::Projects) => " ↑↓ move · Space tick · Enter menu · → sessions · M memory · d delete · m move · c copy · / filter · o orphans · s sort · ? help · q quit".to_string(),
        (_, View::Sessions) => " ↑↓ move · Space tick · a all · s sort · d delete · e export · M memory · / filter · Enter/→ read · Esc/← back · ? help · q quit".to_string(),
        (_, View::Session) => " ↑↓ scroll · PgUp/PgDn page · g/G top/bottom · Esc/← back · ? help · q quit".to_string(),
        (_, View::Memory) => " ↑↓ move · Space tick · Enter/→ read · e edit · s sort · d delete · x export · S sessions · / filter · Esc/← back · ? help · q quit".to_string(),
        (_, View::MemoryFile) => " ↑↓ scroll · PgUp/PgDn page · g/G top/bottom · e edit · Esc/← back · ? help · q quit".to_string(),
    };
    let foot_text = if app.status.is_empty() {
        let mut spans = vec![Span::raw(" ")];
        for (i, action) in hint.split('·').enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", ink(MUTED)));
            }
            let (key, description) = action.trim().split_once(' ').unwrap_or((action.trim(), ""));
            spans.push(Span::styled(key.to_string(), ink(CYAN).bold()));
            spans.push(Span::styled(format!(" {description}"), ink(MUTED)));
        }
        Line::from(spans)
    } else {
        let color = if ["Refused", "Failed", "Delete failed", "Export failed", "Auto-refresh failed"].iter().any(|prefix| app.status.starts_with(prefix)) {
            RED
        } else {
            GREEN
        };
        Line::from(Span::styled(format!(" {}", app.status), ink(color)))
    };
    f.render_widget(Paragraph::new(foot_text).style(surface(TEXT, PANEL)), foot);

    match &app.mode {
        Mode::Confirm { question, effect } => {
            let area = centered(f.area(), 70, 11);
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
                Line::from("Press y to delete · n or Esc to cancel").style(ink(YELLOW)),
            ];
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(dialog(" Delete ".into(), RED)), area);
        }
        Mode::Notice(msg) => {
            let area = centered(f.area(), 70, 8);
            f.render_widget(Clear, area);
            let text = vec![Line::from(msg.as_str()).style(Style::new().add_modifier(Modifier::BOLD)), Line::from(""), Line::from("Nothing was deleted. Press any key to continue.").style(ink(YELLOW))];
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(dialog(" Not done ".into(), RED)), area);
        }
        Mode::Input { label, text, .. } => {
            let area = centered(f.area(), 80, 5);
            f.render_widget(Clear, area);
            let lines = vec![Line::from(format!("{text}▏")), Line::from(""), Line::from("Enter confirm · Esc cancel").style(ink(MUTED))];
            f.render_widget(Paragraph::new(lines).block(dialog(format!(" {label} "), CYAN)), area);
        }
        Mode::Menu { row } => {
            let Some(p) = app.rows.get(*row) else { return };
            let area = centered(f.area(), 64, 11);
            f.render_widget(Clear, area);
            let off = if p.orphan { ink(MUTED) } else { ink(GREEN) };
            let mem = if p.memory.is_empty() { "[m] Auto-memory (none)".to_string() } else { format!("[m] Auto-memory ({} file(s)): read, edit, delete", p.memory.len()) };
            let text = vec![
                Line::from("[s] Browse sessions").style(ink(CYAN)),
                Line::from("[o] Sort projects").style(ink(YELLOW)),
                Line::from(mem).style(if p.memory.is_empty() { ink(MUTED) } else { ink(PURPLE) }),
                Line::from("[h] Shell here (exit returns to this list)").style(off),
                Line::from("[c] Claude here (exit returns to this list)").style(off),
                Line::from("[x] Quit and cd here (see README: shell wrapper)").style(off),
                Line::from(if p.orphan { "    directory no longer exists" } else { "" }).style(ink(RED)),
                Line::from("Esc cancel").style(ink(MUTED)),
            ];
            f.render_widget(Paragraph::new(text).block(dialog(format!(" {} ", directory_label(&p.cwd)), CYAN)), area);
        }
        Mode::SortMenu { cursor, order } => {
            let mut lines = vec![Line::from("Choose a column").style(ink(MUTED))];
            for (i, field) in app.sort_columns().iter().enumerate() {
                let style = if i == *cursor {
                    if crate::style::enabled() { surface(CYAN, SELECTED).bold() } else { Style::new().add_modifier(Modifier::REVERSED) }
                } else { ink(TEXT) };
                lines.push(Line::styled(format!(" {} {}", if i == *cursor { "›" } else { " " }, field.label()), style));
            }
            lines.push(Line::from(""));
            let mut directions = Vec::new();
            for (key, value) in [("a", SortOrder::Ascending), ("d", SortOrder::Descending), ("o", SortOrder::Original)] {
                directions.push(Span::styled(format!(" [{key}] {} ", value.label()), if order == &value { ink(GREEN).bold().underlined() } else { ink(MUTED) }));
            }
            lines.push(Line::from(directions));
            lines.push(Line::from("Original restores this list's default order.").style(ink(MUTED)));
            lines.push(Line::from("↑↓ column · ←→ order · Enter apply · Esc cancel").style(ink(CYAN)));
            let area = centered(f.area(), 58, lines.len() as u16 + 2);
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(lines).block(dialog(" Sort ".into(), CYAN)), area);
        }
        Mode::Help => {
            let (name, keys) = help_lines(app);
            let kw = keys.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
            let mut lines: Vec<Line> = keys
                .iter()
                .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<kw$}  "), ink(CYAN)), Span::raw(*d)]))
                .collect();
            if matches!(app.view, View::Projects | View::Sessions | View::Memory) {
                lines.push(Line::from(""));
                lines.push(Line::from("Deleting is refused while Claude Code runs in that directory.").style(ink(MUTED)));
            }
            let w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 4;
            let area = centered(f.area(), w.max(40), lines.len() as u16 + 2);
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(lines).block(dialog(format!(" Help — {name} — any key closes "), PURPLE)), area);
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
        assert!(out.contains("/x/project") && out.contains("gone") && out.contains("2.0 MB") && out.contains(&when(&Some("2026-01-02T03:04:05Z".into()))), "{out}");
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
        assert!(out.contains(&format!("User · {}", when(&Some("2026-01-02T03:04:05Z".into())))) && out.contains("how are you") && out.contains("Assistant") && out.contains('你') && out.contains('很'), "{out}");
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
        assert!(top.contains("…") && top.contains("deep/project-name") && top.contains("sort: last used"), "{top}");
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
