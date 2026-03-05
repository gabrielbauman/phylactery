mod app;
mod client;
mod events;
mod ui;

use anyhow::Context;
use app::{Action, App};
use crossterm::{
    event::DisableMouseCapture,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use events::AppEvent;
use ratatui::prelude::*;
use std::io;
use uuid::Uuid;

fn main() {
    // Install panic hook that restores the terminal before printing.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        original_hook(info);
    }));

    if let Err(e) = run() {
        let _ = restore_terminal();
        eprintln!("phyl-tui: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new().context("failed to create async runtime")?;
    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    // Terminal setup.
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).context("failed to enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    // Event channels.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AppEvent>(256);
    let (log_target_tx, log_target_rx) = tokio::sync::watch::channel::<Option<Uuid>>(None);

    // Spawn background tasks.
    events::spawn_terminal_reader(tx.clone());
    events::spawn_session_poller(tx.clone());
    events::spawn_feed_reader(tx.clone());
    events::spawn_schedule_scanner(tx.clone());
    events::spawn_log_tailer(tx.clone(), log_target_rx);

    let mut app = App::new();
    let socket = client::socket_path();

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        if let Some(event) = rx.recv().await
            && let Some(action) = app.update(event)
        {
            match action {
                Action::Quit => break,
                Action::CreateSession(prompt) => {
                    let s = socket.clone();
                    tokio::spawn(async move {
                        let body = serde_json::json!({"prompt": prompt}).to_string();
                        let _ = client::post(&s, "/sessions", &body).await;
                    });
                }
                Action::SendMessage(id, msg) => {
                    let s = socket.clone();
                    tokio::spawn(async move {
                        let body = serde_json::json!({"content": msg}).to_string();
                        let path = format!("/sessions/{id}/events");
                        let _ = client::post(&s, &path, &body).await;
                    });
                }
                Action::AnswerQuestion(id, qid, answer) => {
                    let s = socket.clone();
                    tokio::spawn(async move {
                        let body = serde_json::json!({
                            "question_id": qid,
                            "content": answer,
                        })
                        .to_string();
                        let path = format!("/sessions/{id}/events");
                        let _ = client::post(&s, &path, &body).await;
                    });
                }
                Action::StopSession(id) => {
                    let s = socket.clone();
                    tokio::spawn(async move {
                        let path = format!("/sessions/{id}");
                        let _ = client::delete(&s, &path).await;
                    });
                }
                Action::SwitchToChat(id) => {
                    let _ = log_target_tx.send(Some(id));
                }
                Action::SwitchToDashboard => {
                    let _ = log_target_tx.send(None);
                }
            }
        }
    }

    restore_terminal()?;
    Ok(())
}

fn restore_terminal() -> anyhow::Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)
        .context("failed to leave alt screen")?;
    Ok(())
}
