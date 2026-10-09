use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap};

use super::app::{App, Mode, View};
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

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());

    let place = match app.view {
        View::Projects => format!("{} project(s)", app.visible_projects().len()),
        View::Sessions => app.rows.get(app.proj).map(|p| p.cwd.display().to_string()).unwrap_or_default(),
        View::Session => app
            .rows
            .get(app.proj)
            .map(|p| {
                let s = app.visible_sessions().get(app.list_cursor()).map(|i| &p.sessions[*i]);
                format!("{} › {} {}", p.cwd.display(), s.map(|s| s.id.chars().take(8).collect::<String>()).unwrap_or_default(), s.and_then(|s| s.title.clone()).unwrap_or_default())
            })
            .unwrap_or_default(),
    };
    let mut title = vec![
        Span::styled(" claude-sessions ", Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        Span::raw(place),
        Span::styled(format!("  sort: {}", app.sort.label()), Style::new().fg(Color::DarkGray)),
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
                        r.messages.to_string(),
                        mb(r.bytes),
                        when(&r.last),
                    ])
                    .style(style)
                })
                .collect();
            let widths = [Constraint::Length(3), Constraint::Length(4), Constraint::Fill(1), Constraint::Length(8), Constraint::Length(7), Constraint::Length(9), Constraint::Length(16)];
            let header = Row::new(["", "", "DIRECTORY", "SESSIONS", "MSGS", "SIZE", "LAST (UTC)"]).style(Style::new().add_modifier(Modifier::BOLD));
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
        View::Session => {}
    }
    if app.view == View::Session {
        let inner = Rect { y: body.y + 1, height: body.height.saturating_sub(1), ..body };
        let lines = session_lines(&app.turns, inner.width.saturating_sub(1) as usize);
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
        (_, View::Projects) => " ↑↓ move · Space tick · a all · Enter menu · → sessions · d delete · m move · c copy · / filter · o orphans · s sort · ? help · q quit".to_string(),
        (_, View::Sessions) => " ↑↓ move · Space tick · a all · d delete · e export · / filter · Enter/→ read · Esc/← back · ? help · q quit".to_string(),
        (_, View::Session) => " ↑↓ scroll · PgUp/PgDn page · g/G top/bottom · Esc/← back · ? help · q quit".to_string(),
    };
    let foot_text = if app.status.is_empty() { Span::styled(hint, Style::new().fg(Color::DarkGray)) } else { Span::styled(format!(" {}", app.status), Style::new().fg(Color::Yellow)) };
    f.render_widget(Paragraph::new(Line::from(foot_text)), foot);

    match &app.mode {
        Mode::Confirm { question, .. } => {
            let area = centered(f.area(), 70, 8);
            f.render_widget(Clear, area);
            let cfg = if app.purge_config { "ON  (history.jsonl lines + .claude.json entry are removed; backed up first)" } else { "off (prompt history and trust settings are kept)" };
            let text = vec![
                Line::from(question.as_str()).style(Style::new().add_modifier(Modifier::BOLD)),
                Line::from(""),
                Line::from(format!("[p] also purge config: {cfg}")),
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
            let area = centered(f.area(), 64, 8);
            f.render_widget(Clear, area);
            let off = if p.orphan { Style::new().fg(Color::DarkGray) } else { Style::new() };
            let text = vec![
                Line::from("[s] Browse sessions"),
                Line::from("[h] Shell here (exit returns to this list)").style(off),
                Line::from("[x] Quit and cd here (see README: shell wrapper)").style(off),
                Line::from(if p.orphan { "    directory no longer exists" } else { "" }).style(Style::new().fg(Color::Red)),
                Line::from("Esc cancel").style(Style::new().fg(Color::DarkGray)),
            ];
            f.render_widget(Paragraph::new(text).block(Block::bordered().title(format!(" {} ", p.cwd.display()))), area);
        }
        Mode::Help => {
            let area = centered(f.area(), 70, 21);
            f.render_widget(Clear, area);
            let text = "\
↑↓ / j k    move          g G / Home End   first / last
Space       tick + next   a                tick all / none
Enter       project: menu (sessions / shell / cd) · session: read
→           open sessions    Esc / ←  back (Esc: clear / quit)
Reading:    ↑↓ j k scroll · PgUp PgDn b Space page · g G top / bottom
/           filter        o                orphans only
s           cycle sort (path, size, last used)
d           delete ticked (or the row under the cursor)
            p in the dialog: also purge history/.claude.json
m  c        move / copy the project's directory + sessions
e           export the session as Markdown
r           reload        q                quit

Deleting is refused while Claude Code runs in that directory.";
            f.render_widget(Paragraph::new(text).block(Block::bordered().title(" Help — any key closes ")), area);
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
        assert!(out.contains("Help") && out.contains("orphans only") && out.contains("Space"), "{out}");
        a.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!render(&a).contains("Help"));
    }

    #[test]
    fn enter_shows_directory_menu() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let out = render(&a);
        assert!(out.contains("[s] Browse sessions") && out.contains("[h] Shell here") && out.contains("[x] Quit and cd") && out.contains("no longer exists"), "{out}");
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
    fn session_view_lists_title() {
        let mut a = app();
        a.handle_key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Right, ratatui::crossterm::event::KeyModifiers::NONE));
        let out = render(&a);
        assert!(out.contains("Fix the bug") && out.contains("abcdef12"), "{out}");
    }
}
