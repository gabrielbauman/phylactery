//! Rendering functions for the TUI dashboard and chat views.

use crate::app::{App, FeedKind, InputMode, ListItem, View};
use crate::events::relative_time;
use phyl_core::{LogEntry, LogEntryType, SessionStatus};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem as RatatuiListItem, Paragraph, Wrap};

/// Main draw function — dispatches to dashboard or chat renderer.
pub fn draw(f: &mut Frame, app: &App) {
    match &app.view {
        View::Dashboard => draw_dashboard(f, app),
        View::Chat(id) => draw_chat(f, app, *id),
    }
}

// ────────────────────────────── Dashboard ──────────────────────────────

fn draw_dashboard(f: &mut Frame, app: &App) {
    let area = f.area();

    // Layout: title (1) + sessions (%) + activity (%) + status (1).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // Title bar
            Constraint::Min(5),     // Session list (flex)
            Constraint::Length(10), // Activity feed
            Constraint::Length(1),  // Status bar
        ])
        .split(area);

    draw_title_bar(f, app, chunks[0]);
    draw_session_list(f, app, chunks[1]);
    draw_activity_feed(f, app, chunks[2]);
    draw_dashboard_status_bar(f, app, chunks[3]);
}

fn draw_title_bar(f: &mut Frame, app: &App, area: Rect) {
    let status = if app.daemon_ok {
        format!("{} active", app.daemon_active)
    } else {
        "disconnected".to_string()
    };

    let status_style = if app.daemon_ok {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };

    let title = Line::from(vec![
        Span::styled("PHYLACTERY", Style::default().bold()),
        Span::raw(" "),
        Span::styled(
            "\u{2500}".repeat(area.width.saturating_sub(14 + status.len() as u16 + 4) as usize),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(status, status_style),
        Span::raw(" "),
    ]);

    f.render_widget(Paragraph::new(title), area);
}

fn draw_session_list(f: &mut Frame, app: &App, area: Rect) {
    if app.list_items.is_empty() {
        let empty = Paragraph::new("  No sessions. Press 'n' to create one.")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        f.render_widget(empty, area);
        return;
    }

    let items: Vec<RatatuiListItem> = app
        .list_items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let (icon, id_str, status_str, summary, time_str, style) = match item {
                ListItem::Session(s) => {
                    let is_asking = app.session_is_asking(&s.id);
                    let short_id = s.id.to_string()[..8].to_string();

                    let (icon, status_text, st) = if is_asking {
                        (
                            "?",
                            "asking ",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        match s.status {
                            SessionStatus::Running => {
                                (">", "running", Style::default().fg(Color::Green))
                            }
                            SessionStatus::Done => {
                                (" ", "done   ", Style::default().fg(Color::DarkGray))
                            }
                            SessionStatus::Crashed => {
                                ("!", "crashed", Style::default().fg(Color::Red))
                            }
                            SessionStatus::TimedOut => {
                                ("!", "timeout", Style::default().fg(Color::Red))
                            }
                        }
                    };

                    let summary_text = s
                        .summary
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(50)
                        .collect::<String>();

                    let time = relative_time(s.created_at);

                    (
                        icon,
                        short_id,
                        status_text.to_string(),
                        summary_text,
                        time,
                        st,
                    )
                }
                ListItem::Scheduled(e) => {
                    let short_id = e.id.to_string()[..8].to_string();
                    let summary_text = e.prompt.chars().take(50).collect::<String>();
                    let time = relative_time(e.at);
                    let st = Style::default().fg(Color::Cyan);
                    ("~", short_id, "sched  ".to_string(), summary_text, time, st)
                }
            };

            let width = area.width as usize;
            // Layout: " {icon} {id}  {status}  {summary}  {time} "
            let fixed_len = 1 + 1 + 1 + 8 + 2 + 7 + 2 + 2 + time_str.len() + 1;
            let summary_max = width.saturating_sub(fixed_len);
            let truncated_summary: String = summary.chars().take(summary_max).collect();

            let padding = " ".repeat(summary_max.saturating_sub(truncated_summary.chars().count()));

            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled(icon.to_string(), style),
                Span::raw(" "),
                Span::styled(id_str, style),
                Span::raw("  "),
                Span::styled(status_str, style),
                Span::raw("  "),
                Span::raw(truncated_summary),
                Span::raw(padding),
                Span::styled(
                    format!("  {time_str}"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" "),
            ]);

            let item_style = if i == app.selected_index {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            RatatuiListItem::new(line).style(item_style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::TOP)
            .title(" Sessions ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    f.render_widget(list, area);
}

fn draw_activity_feed(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<RatatuiListItem> = app
        .feed
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|item| {
            let ts = item.ts.format("%H:%M");
            let style = match item.kind {
                FeedKind::Question => Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                FeedKind::Done => Style::default().fg(Color::DarkGray),
                FeedKind::Error => Style::default().fg(Color::Red),
            };

            let line = Line::from(vec![
                Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(&item.short_id, style),
                Span::raw(" "),
                Span::styled(&item.summary, style),
            ]);

            RatatuiListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::TOP)
            .title(" Activity ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    f.render_widget(list, area);
}

fn draw_dashboard_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let content = match app.input_mode {
        InputMode::Normal => {
            let hints = " j/k:\u{2191}\u{2193}  Enter:open  n:new  s:stop  q:quit";
            if let Some(ref err) = app.daemon_error {
                Line::from(vec![
                    Span::styled(format!(" {err}"), Style::default().fg(Color::Red)),
                    Span::raw("  "),
                    Span::styled(hints, Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray)))
            }
        }
        InputMode::NewSession => {
            let cursor = if app.tick % 4 < 2 { "\u{2588}" } else { " " };
            Line::from(vec![
                Span::styled(" New session: ", Style::default().fg(Color::Cyan)),
                Span::raw(&app.new_session_input),
                Span::raw(cursor),
                Span::styled(
                    "  (Enter:create  Esc:cancel)",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
    };

    f.render_widget(Paragraph::new(content), area);
}

// ────────────────────────────── Chat View ──────────────────────────────

fn draw_chat(f: &mut Frame, app: &App, session_id: uuid::Uuid) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Header
            Constraint::Min(3),    // Chat log
            Constraint::Length(1), // Input
            Constraint::Length(1), // Status bar
        ])
        .split(area);

    draw_chat_header(f, app, session_id, chunks[0]);
    draw_chat_log(f, app, chunks[1]);
    draw_chat_input(f, app, chunks[2]);
    draw_chat_status_bar(f, app, chunks[3]);
}

fn draw_chat_header(f: &mut Frame, app: &App, session_id: uuid::Uuid, area: Rect) {
    let short_id = &session_id.to_string()[..8];
    let status_str = format!("{:?}", app.chat_status).to_lowercase();
    let status_style = match app.chat_status {
        SessionStatus::Running => Style::default().fg(Color::Green),
        SessionStatus::Done => Style::default().fg(Color::DarkGray),
        SessionStatus::Crashed | SessionStatus::TimedOut => Style::default().fg(Color::Red),
    };

    let header = vec![
        Line::from(vec![
            Span::styled(format!(" {short_id}"), Style::default().bold()),
            Span::raw("  "),
            Span::styled(status_str, status_style),
        ]),
        Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                truncate_str(&app.chat_prompt, area.width.saturating_sub(2) as usize),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    f.render_widget(Paragraph::new(header).block(block), area);
}

fn draw_chat_log(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for entry in &app.chat_log {
        let entry_lines = format_log_entry_styled(entry, area.width as usize);
        lines.extend(entry_lines);
    }

    // Add "working..." indicator if running with no pending question.
    if app.chat_status == SessionStatus::Running && app.current_pending_question().is_none() {
        let dots = match app.tick % 4 {
            0 => ".  ",
            1 => ".. ",
            2 => "...",
            _ => "   ",
        };
        lines.push(Line::from(Span::styled(
            format!("                              [working{dots}]"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Calculate scroll.
    let visible_height = area.height as usize;
    let total_lines = lines.len();
    let scroll = if app.auto_scroll {
        total_lines.saturating_sub(visible_height)
    } else {
        total_lines
            .saturating_sub(visible_height)
            .saturating_sub(app.scroll_offset)
    };

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    f.render_widget(paragraph, area);
}

fn draw_chat_input(f: &mut Frame, app: &App, area: Rect) {
    let cursor = if app.tick % 4 < 2 { "\u{2588}" } else { " " };
    let line = Line::from(vec![
        Span::styled(" > ", Style::default().fg(Color::Cyan).bold()),
        Span::raw(&app.input_buffer),
        Span::raw(cursor),
    ]);

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));

    f.render_widget(Paragraph::new(line).block(block), area);
}

fn draw_chat_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let hints = if app.current_pending_question().is_some() {
        " Esc:back  1-9:answer  Enter:send"
    } else {
        " Esc:back  Enter:send  PgUp/PgDn:scroll"
    };

    let line = Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray)));

    f.render_widget(Paragraph::new(line), area);
}

// ────────────────────────────── Log Formatting ──────────────────────────────

fn format_log_entry_styled(entry: &LogEntry, _width: usize) -> Vec<Line<'static>> {
    let ts = entry.ts.format("%H:%M:%S").to_string();
    let mut lines = Vec::new();

    match entry.entry_type {
        LogEntryType::User => {
            if let Some(ref content) = entry.content {
                lines.push(Line::from(vec![
                    Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                    Span::styled("user: ", Style::default().fg(Color::Blue).bold()),
                    Span::raw(content.clone()),
                ]));
            }
        }
        LogEntryType::Assistant => {
            if let Some(ref content) = entry.content {
                lines.push(Line::from(vec![
                    Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                    Span::styled("assistant: ", Style::default().fg(Color::White).bold()),
                    Span::raw(content.clone()),
                ]));
            }
            for tc in &entry.tool_calls {
                lines.push(Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!("-> {}({})", tc.name, tc.id),
                        Style::default().fg(Color::Cyan).dim(),
                    ),
                ]));
            }
        }
        LogEntryType::ToolResult => {
            let id = entry.tool_call_id.as_deref().unwrap_or("?");
            if let Some(ref content) = entry.content {
                let display = if content.len() > 200 {
                    format!("{}...", &content[..200])
                } else {
                    content.clone()
                };
                lines.push(Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!("<- [{id}]: {display}"),
                        Style::default().fg(Color::DarkGray).dim(),
                    ),
                ]));
            }
        }
        LogEntryType::Question => {
            let qid = entry.id.as_deref().unwrap_or("?");
            let text = entry.content.as_deref().unwrap_or("");
            lines.push(Line::from(vec![
                Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("? QUESTION [{qid}]: {text}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            for (i, opt) in entry.options.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!("  {}: {opt}", i + 1),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));
            }
        }
        LogEntryType::Answer => {
            let qid = entry.question_id.as_deref().unwrap_or("?");
            let text = entry.content.as_deref().unwrap_or("");
            lines.push(Line::from(vec![
                Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("  answer[{qid}]: {text}"),
                    Style::default().fg(Color::Green),
                ),
            ]));
        }
        LogEntryType::Done => {
            let summary = entry
                .summary
                .as_deref()
                .or(entry.content.as_deref())
                .unwrap_or("(no summary)");
            lines.push(Line::from(vec![
                Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("DONE: {summary}"),
                    Style::default().fg(Color::Green).bold(),
                ),
            ]));
        }
        LogEntryType::Error => {
            let msg = entry.content.as_deref().unwrap_or("unknown error");
            lines.push(Line::from(vec![
                Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("ERROR: {msg}"),
                    Style::default().fg(Color::Red).bold(),
                ),
            ]));
        }
        LogEntryType::System => {
            if let Some(ref content) = entry.content {
                lines.push(Line::from(vec![
                    Span::styled(format!(" [{ts}] "), Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("system: {content}"),
                        Style::default().fg(Color::DarkGray).dim(),
                    ),
                ]));
            }
        }
    }

    lines
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}
