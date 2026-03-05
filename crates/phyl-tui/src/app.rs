//! Application state and update logic for the TUI.

use crate::events::AppEvent;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use phyl_core::{LogEntry, LogEntryType, ScheduleEntry, SessionInfo, SessionStatus};
use std::collections::HashMap;
use uuid::Uuid;

/// Which view the TUI is showing.
#[derive(Clone, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Chat(Uuid),
}

/// An item in the unified session list.
pub enum ListItem {
    Session(SessionInfo),
    Scheduled(ScheduleEntry),
}

/// A pending question awaiting human answer.
#[derive(Clone)]
#[allow(dead_code)]
pub struct PendingQuestion {
    pub question_id: String,
    pub text: String,
    pub options: Vec<String>,
}

/// An entry in the global activity feed.
#[allow(dead_code)]
pub struct FeedItem {
    pub ts: DateTime<Utc>,
    pub session_id: Uuid,
    pub short_id: String,
    pub summary: String,
    pub kind: FeedKind,
}

#[derive(Clone, PartialEq, Eq)]
pub enum FeedKind {
    Question,
    Done,
    Error,
}

/// Side-effect actions produced by key handling.
pub enum Action {
    Quit,
    CreateSession(String),
    SendMessage(Uuid, String),
    AnswerQuestion(Uuid, String, String),
    StopSession(Uuid),
    SwitchToChat(Uuid),
    SwitchToDashboard,
}

/// Whether the dashboard input is active (creating a new session).
pub enum InputMode {
    Normal,
    NewSession,
}

pub struct App {
    // View.
    pub view: View,

    // Dashboard state.
    pub sessions: Vec<SessionInfo>,
    pub schedule: Vec<ScheduleEntry>,
    pub list_items: Vec<ListItem>,
    pub selected_index: usize,
    pub feed: Vec<FeedItem>,
    pub input_mode: InputMode,
    pub new_session_input: String,

    // Sessions with pending questions (detected from SSE feed).
    pub asking_sessions: HashMap<Uuid, PendingQuestion>,

    // Chat state.
    pub chat_log: Vec<LogEntry>,
    pub chat_prompt: String,
    pub chat_status: SessionStatus,
    pub input_buffer: String,
    pub scroll_offset: usize,
    pub auto_scroll: bool,

    // Daemon health.
    pub daemon_ok: bool,
    pub daemon_active: usize,
    pub daemon_error: Option<String>,

    // Animation tick counter.
    pub tick: usize,
}

impl App {
    pub fn new() -> Self {
        Self {
            view: View::Dashboard,
            sessions: Vec::new(),
            schedule: Vec::new(),
            list_items: Vec::new(),
            selected_index: 0,
            feed: Vec::new(),
            input_mode: InputMode::Normal,
            new_session_input: String::new(),
            asking_sessions: HashMap::new(),
            chat_log: Vec::new(),
            chat_prompt: String::new(),
            chat_status: SessionStatus::Running,
            input_buffer: String::new(),
            scroll_offset: 0,
            auto_scroll: true,
            daemon_ok: false,
            daemon_active: 0,
            daemon_error: None,
            tick: 0,
        }
    }

    /// Process an event and optionally return a side-effect action.
    pub fn update(&mut self, event: AppEvent) -> Option<Action> {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                None
            }
            AppEvent::SessionsUpdated(sessions) => {
                self.sessions = sessions;
                // Update chat status if we're viewing a session.
                if let View::Chat(id) = &self.view
                    && let Some(s) = self.sessions.iter().find(|s| s.id == *id)
                {
                    self.chat_status = s.status.clone();
                }
                self.rebuild_list();
                None
            }
            AppEvent::FeedEvent { session_id, entry } => {
                self.handle_feed_event(session_id, entry);
                None
            }
            AppEvent::ScheduleUpdated(entries) => {
                self.schedule = entries;
                self.rebuild_list();
                None
            }
            AppEvent::LogEntries {
                session_id,
                entries,
            } => {
                if let View::Chat(id) = &self.view
                    && *id == session_id
                {
                    self.chat_log.extend(entries);
                    // Detect pending questions from the log.
                    self.detect_pending_question_from_log();
                    if self.auto_scroll {
                        // Auto-scroll to bottom.
                        self.scroll_to_bottom();
                    }
                }
                None
            }
            AppEvent::DaemonStatus { ok, active } => {
                self.daemon_ok = ok;
                self.daemon_active = active;
                if ok {
                    self.daemon_error = None;
                }
                None
            }
            AppEvent::DaemonError(msg) => {
                self.daemon_error = Some(msg);
                None
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }

        match &self.view {
            View::Dashboard => self.handle_key_dashboard(key),
            View::Chat(id) => {
                let id = *id;
                self.handle_key_chat(key, id)
            }
        }
    }

    fn handle_key_dashboard(&mut self, key: KeyEvent) -> Option<Action> {
        // If in new session input mode.
        if let InputMode::NewSession = self.input_mode {
            return self.handle_key_new_session(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.list_items.is_empty() {
                    self.selected_index = (self.selected_index + 1) % self.list_items.len();
                }
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.list_items.is_empty() {
                    self.selected_index = if self.selected_index == 0 {
                        self.list_items.len() - 1
                    } else {
                        self.selected_index - 1
                    };
                }
                None
            }
            KeyCode::Enter => {
                if let Some(item) = self.list_items.get(self.selected_index) {
                    match item {
                        ListItem::Session(s) => {
                            let id = s.id;
                            // Reset chat state.
                            self.chat_log.clear();
                            self.input_buffer.clear();
                            self.scroll_offset = 0;
                            self.auto_scroll = true;
                            self.chat_prompt = s
                                .summary
                                .clone()
                                .unwrap_or_else(|| "(no summary)".to_string());
                            self.chat_status = s.status.clone();
                            self.view = View::Chat(id);
                            return Some(Action::SwitchToChat(id));
                        }
                        ListItem::Scheduled(_) => {
                            // Can't open scheduled sessions.
                        }
                    }
                }
                None
            }
            KeyCode::Char('n') => {
                self.input_mode = InputMode::NewSession;
                self.new_session_input.clear();
                None
            }
            KeyCode::Char('s') => {
                if let Some(ListItem::Session(s)) = self.list_items.get(self.selected_index)
                    && s.status == SessionStatus::Running
                {
                    return Some(Action::StopSession(s.id));
                }
                None
            }
            _ => None,
        }
    }

    fn handle_key_new_session(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.new_session_input.clear();
                None
            }
            KeyCode::Enter => {
                let prompt = self.new_session_input.trim().to_string();
                self.input_mode = InputMode::Normal;
                self.new_session_input.clear();
                if !prompt.is_empty() {
                    Some(Action::CreateSession(prompt))
                } else {
                    None
                }
            }
            KeyCode::Char(c) => {
                self.new_session_input.push(c);
                None
            }
            KeyCode::Backspace => {
                self.new_session_input.pop();
                None
            }
            _ => None,
        }
    }

    fn handle_key_chat(&mut self, key: KeyEvent, session_id: Uuid) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.view = View::Dashboard;
                Some(Action::SwitchToDashboard)
            }
            KeyCode::Enter => {
                let msg = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                if !msg.is_empty() {
                    Some(Action::SendMessage(session_id, msg))
                } else {
                    None
                }
            }
            KeyCode::Char(c) => {
                // Check if this is a quick answer (1-9) when a question is pending
                // and the input buffer is empty.
                if self.input_buffer.is_empty()
                    && let Some(pq) = self.asking_sessions.get(&session_id)
                    && let Some(digit) = c.to_digit(10)
                {
                    let idx = digit as usize;
                    if idx >= 1 && idx <= pq.options.len() {
                        let answer = pq.options[idx - 1].clone();
                        let qid = pq.question_id.clone();
                        return Some(Action::AnswerQuestion(session_id, qid, answer));
                    }
                }
                self.input_buffer.push(c);
                None
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                None
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
                self.auto_scroll = false;
                None
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                if self.scroll_offset == 0 {
                    self.auto_scroll = true;
                }
                None
            }
            _ => None,
        }
    }

    fn handle_feed_event(&mut self, session_id: Uuid, entry: LogEntry) {
        let short_id = session_id.to_string()[..8].to_string();

        let (summary, kind) = match entry.entry_type {
            LogEntryType::Question => {
                let qid = entry.id.clone().unwrap_or_default();
                let text = entry.content.clone().unwrap_or_default();
                let options = entry.options.clone();
                self.asking_sessions.insert(
                    session_id,
                    PendingQuestion {
                        question_id: qid,
                        text: text.clone(),
                        options,
                    },
                );
                (format!("QUESTION: {text}"), FeedKind::Question)
            }
            LogEntryType::Done => {
                self.asking_sessions.remove(&session_id);
                let text = entry
                    .summary
                    .clone()
                    .or(entry.content.clone())
                    .unwrap_or_else(|| "(no summary)".to_string());
                (format!("DONE: {text}"), FeedKind::Done)
            }
            LogEntryType::Error => {
                let text = entry
                    .content
                    .clone()
                    .unwrap_or_else(|| "unknown error".to_string());
                (format!("ERROR: {text}"), FeedKind::Error)
            }
            _ => return,
        };

        self.feed.push(FeedItem {
            ts: entry.ts,
            session_id,
            short_id,
            summary,
            kind,
        });

        // Cap feed at 200 entries.
        if self.feed.len() > 200 {
            self.feed.drain(..self.feed.len() - 200);
        }

        self.rebuild_list();
    }

    fn detect_pending_question_from_log(&mut self) {
        if let View::Chat(id) = &self.view {
            let id = *id;
            // Find the last Question that has no subsequent Answer.
            let mut last_question: Option<PendingQuestion> = None;

            for entry in &self.chat_log {
                match entry.entry_type {
                    LogEntryType::Question => {
                        last_question = Some(PendingQuestion {
                            question_id: entry.id.clone().unwrap_or_default(),
                            text: entry.content.clone().unwrap_or_default(),
                            options: entry.options.clone(),
                        });
                    }
                    LogEntryType::Answer => {
                        if let Some(ref q) = last_question
                            && entry.question_id.as_deref() == Some(&q.question_id)
                        {
                            last_question = None;
                        }
                    }
                    _ => {}
                }
            }

            if let Some(pq) = last_question {
                self.asking_sessions.insert(id, pq);
            } else {
                self.asking_sessions.remove(&id);
            }
        }
    }

    /// Sort sessions by urgency and merge in scheduled entries.
    pub fn rebuild_list(&mut self) {
        let mut items: Vec<ListItem> = Vec::new();

        // 1. Sessions with pending questions.
        let mut asking: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| self.asking_sessions.contains_key(&s.id))
            .collect();
        asking.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        for s in asking {
            items.push(ListItem::Session(s.clone()));
        }

        // 2. Running sessions (not asking).
        let mut running: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| {
                s.status == SessionStatus::Running && !self.asking_sessions.contains_key(&s.id)
            })
            .collect();
        running.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        for s in running {
            items.push(ListItem::Session(s.clone()));
        }

        // 3. Scheduled entries (sorted by fire time, soonest first).
        for e in &self.schedule {
            items.push(ListItem::Scheduled(e.clone()));
        }

        // 4. Done sessions.
        let mut done: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Done)
            .collect();
        done.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        for s in done {
            items.push(ListItem::Session(s.clone()));
        }

        // 5. Crashed / Timed out sessions.
        let mut failed: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Crashed || s.status == SessionStatus::TimedOut)
            .collect();
        failed.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        for s in failed {
            items.push(ListItem::Session(s.clone()));
        }

        // Adjust selected index.
        if self.selected_index >= items.len() && !items.is_empty() {
            self.selected_index = items.len() - 1;
        }

        self.list_items = items;
    }

    /// Calculate the line count of the chat log and scroll to bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Get the pending question for the currently viewed session, if any.
    pub fn current_pending_question(&self) -> Option<&PendingQuestion> {
        if let View::Chat(id) = &self.view {
            self.asking_sessions.get(id)
        } else {
            None
        }
    }

    /// Check if a session has a pending question.
    pub fn session_is_asking(&self, id: &Uuid) -> bool {
        self.asking_sessions.contains_key(id)
    }
}
