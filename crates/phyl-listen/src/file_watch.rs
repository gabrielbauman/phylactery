//! File watch listener — cross-platform, with glob filtering, debouncing,
//! and rate limiting. Uses the `notify` crate (inotify on Linux, FSEvents on
//! macOS, ReadDirectoryChanges on Windows).

use crate::daemon_client;
use crate::rate_limit::RateLimiter;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use phyl_core::ListenWatchConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

pub async fn run_file_watches(
    watches: Vec<ListenWatchConfig>,
    socket: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let rate_limiter = RateLimiter::new();

    // Channel for receiving notify events
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(256);

    // Create the watcher
    let tx_clone = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            let _ = tx_clone.blocking_send(event);
        }
    })
    .map_err(|e| format!("failed to create file watcher: {e}"))?;

    // Map watched paths to config indices
    let mut path_config: HashMap<PathBuf, usize> = HashMap::new();

    for (i, watch_config) in watches.iter().enumerate() {
        let path = Path::new(&watch_config.path);
        if !path.exists() {
            eprintln!(
                "phyl-listen: [{}] path does not exist: {}, will retry",
                watch_config.name, watch_config.path
            );
            continue;
        }

        let mode = if watch_config.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        match watcher.watch(path, mode) {
            Ok(()) => {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                path_config.insert(canonical, i);
                eprintln!(
                    "phyl-listen: [{}] watching {}",
                    watch_config.name, watch_config.path
                );
            }
            Err(e) => {
                eprintln!(
                    "phyl-listen: [{}] failed to watch {}: {e}",
                    watch_config.name, watch_config.path
                );
            }
        }
    }

    if path_config.is_empty() {
        return Err("no watches could be established".to_string());
    }

    // Debounce state: path -> (last event time, event type, watch config index)
    let mut debounce_state: HashMap<PathBuf, (Instant, String, usize)> = HashMap::new();

    loop {
        // Check debounce timers
        let now = Instant::now();
        let mut to_fire = Vec::new();
        debounce_state.retain(|path, (last_time, event_type, config_idx)| {
            let debounce = Duration::from_secs(watches[*config_idx].debounce);
            if now.duration_since(*last_time) >= debounce {
                to_fire.push((path.clone(), event_type.clone(), *config_idx));
                false // remove from debounce state
            } else {
                true // keep
            }
        });

        // Fire debounced events
        for (path, event_type, config_idx) in to_fire {
            let watch_config = &watches[config_idx];

            if !rate_limiter.check(&watch_config.name, watch_config.rate_limit) {
                eprintln!(
                    "phyl-listen: [{}] rate limited, dropping event for {}",
                    watch_config.name,
                    path.display()
                );
                continue;
            }

            let prompt = assemble_file_prompt(watch_config, &path, &event_type);
            match daemon_client::create_session(socket, &prompt).await {
                Ok(id) => {
                    eprintln!("phyl-listen: [{}] session created: {id}", watch_config.name);
                }
                Err(e) => {
                    eprintln!(
                        "phyl-listen: [{}] failed to create session: {e}",
                        watch_config.name
                    );
                }
            }
        }

        // Wait for notify events or debounce timeout
        let sleep = tokio::time::sleep(Duration::from_millis(500));
        tokio::pin!(sleep);

        tokio::select! {
            Some(event) = rx.recv() => {
                // Process the notify event
                let event_type = notify_event_type(&event.kind);
                if event_type.is_empty() {
                    continue;
                }

                for event_path in &event.paths {
                    // Find the matching watch config by checking parent paths
                    let config_idx = match find_config_for_path(event_path, &watches, &path_config) {
                        Some(idx) => idx,
                        None => continue,
                    };
                    let watch_config = &watches[config_idx];

                    // Get the filename
                    let filename = match event_path.file_name() {
                        Some(name) => name.to_string_lossy().to_string(),
                        None => continue,
                    };

                    // Skip hidden files unless glob matches
                    if filename.starts_with('.') {
                        if let Some(glob_pattern) = &watch_config.glob {
                            if !glob_matches(glob_pattern, &filename) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }

                    // Apply glob filter
                    if let Some(glob_pattern) = &watch_config.glob
                        && !glob_matches(glob_pattern, &filename) {
                            continue;
                        }

                    // Check against configured event types
                    if !watch_config.events.is_empty()
                        && !watch_config.events.contains(&event_type)
                    {
                        continue;
                    }

                    // Add to debounce state
                    debounce_state.insert(
                        event_path.clone(),
                        (Instant::now(), event_type.clone(), config_idx),
                    );
                }
            }
            _ = &mut sleep => {
                // Timeout, just loop to check debounce timers
            }
            _ = shutdown.changed() => {
                eprintln!("phyl-listen: file watches stopped");
                return Ok(());
            }
        }
    }
}

/// Map a `notify` event kind to our string event types.
fn notify_event_type(kind: &EventKind) -> String {
    match kind {
        EventKind::Create(_) => "create".to_string(),
        EventKind::Modify(_) => "modify".to_string(),
        EventKind::Remove(_) => "delete".to_string(),
        _ => String::new(),
    }
}

/// Find which watch config a path belongs to by checking parent directories.
fn find_config_for_path(
    event_path: &Path,
    watches: &[ListenWatchConfig],
    path_config: &HashMap<PathBuf, usize>,
) -> Option<usize> {
    // Try exact canonical match first, then walk parent directories
    let canonical = event_path
        .canonicalize()
        .unwrap_or_else(|_| event_path.to_path_buf());

    let mut check = Some(canonical.as_path());
    while let Some(dir) = check {
        if let Some(idx) = path_config.get(dir) {
            return Some(*idx);
        }
        check = dir.parent();
    }

    // Fallback: match against the configured path strings
    for (i, watch) in watches.iter().enumerate() {
        let watch_path = Path::new(&watch.path);
        if event_path.starts_with(watch_path) {
            return Some(i);
        }
    }

    None
}

fn glob_matches(pattern: &str, filename: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| p.matches(filename))
        .unwrap_or(false)
}

fn assemble_file_prompt(config: &ListenWatchConfig, path: &Path, event_type: &str) -> String {
    let ts = chrono::Utc::now().to_rfc3339();

    let mut prompt = format!(
        "{}\n\n=== FILE EVENT ===\nSource: {}\nPath: {}\nEvent: {event_type}\nTimestamp: {ts}",
        config.prompt,
        config.name,
        path.display()
    );

    // Include file size if the file exists
    if let Ok(metadata) = std::fs::metadata(path) {
        prompt.push_str(&format!("\nSize: {} bytes", metadata.len()));

        // Include file content for create/modify on small files
        if (event_type == "create" || event_type == "modify")
            && metadata.len() < 100_000
            && let Ok(content) = std::fs::read_to_string(path)
        {
            prompt.push_str(&format!("\n\n=== FILE CONTENT ===\n{content}"));
        }
        // Skip binary files silently
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matches_simple() {
        assert!(glob_matches("*.eml", "test.eml"));
        assert!(!glob_matches("*.eml", "test.txt"));
    }

    #[test]
    fn test_glob_matches_star() {
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*.conf", "app.conf"));
    }

    #[test]
    fn test_notify_event_type() {
        assert_eq!(
            notify_event_type(&EventKind::Create(notify::event::CreateKind::File)),
            "create"
        );
        assert_eq!(
            notify_event_type(&EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            "modify"
        );
        assert_eq!(
            notify_event_type(&EventKind::Remove(notify::event::RemoveKind::File)),
            "delete"
        );
        assert_eq!(
            notify_event_type(&EventKind::Access(notify::event::AccessKind::Read)),
            ""
        );
    }

    #[test]
    fn test_assemble_file_prompt_basic() {
        let config = ListenWatchConfig {
            name: "test".to_string(),
            path: "/tmp".to_string(),
            prompt: "A file event occurred.".to_string(),
            recursive: false,
            events: vec![],
            glob: None,
            debounce: 2,
            rate_limit: 10,
        };

        let path = Path::new("/tmp/nonexistent_test_file_12345");
        let prompt = assemble_file_prompt(&config, path, "create");
        assert!(prompt.starts_with("A file event occurred."));
        assert!(prompt.contains("=== FILE EVENT ==="));
        assert!(prompt.contains("Source: test"));
        assert!(prompt.contains("Event: create"));
    }

    #[test]
    fn test_watch_config_deserialize() {
        let toml_str = r#"
            [listen]

            [[listen.watch]]
            name = "inbox"
            path = "/home/user/inbox"
            prompt = "New file in inbox."
            recursive = true
            events = ["create"]
            glob = "*.eml"
            debounce = 5
        "#;
        let config: phyl_core::Config = toml::from_str(toml_str).unwrap();
        let listen = config.listen.unwrap();
        assert_eq!(listen.watch.len(), 1);
        let w = &listen.watch[0];
        assert_eq!(w.name, "inbox");
        assert!(w.recursive);
        assert_eq!(w.events, vec!["create"]);
        assert_eq!(w.glob.as_deref(), Some("*.eml"));
        assert_eq!(w.debounce, 5);
    }

    #[test]
    fn test_watch_config_defaults() {
        let toml_str = r#"
            [listen]

            [[listen.watch]]
            name = "test"
            path = "/tmp"
            prompt = "Something changed."
        "#;
        let config: phyl_core::Config = toml::from_str(toml_str).unwrap();
        let listen = config.listen.unwrap();
        let w = &listen.watch[0];
        assert!(!w.recursive);
        assert!(w.events.is_empty());
        assert!(w.glob.is_none());
        assert_eq!(w.debounce, 2); // default
        assert_eq!(w.rate_limit, 10); // default
    }
}
