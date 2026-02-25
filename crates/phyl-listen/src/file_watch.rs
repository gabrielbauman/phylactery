//! File watch listener — inotify-based, with glob filtering, debouncing,
//! and rate limiting.

use crate::daemon_client;
use crate::rate_limit::RateLimiter;
use inotify::{EventMask, Inotify, WatchMask};
use phyl_core::ListenWatchConfig;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

pub async fn run_file_watches(
    watches: Vec<ListenWatchConfig>,
    socket: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let mut inotify = Inotify::init().map_err(|e| format!("failed to init inotify: {e}"))?;
    let rate_limiter = RateLimiter::new();

    // Map watch descriptors to configs
    let mut wd_map: HashMap<i32, usize> = HashMap::new();

    for (i, watch_config) in watches.iter().enumerate() {
        let path = Path::new(&watch_config.path);
        if !path.exists() {
            eprintln!(
                "phyl-listen: [{}] path does not exist: {}, will retry",
                watch_config.name, watch_config.path
            );
            continue;
        }

        let mask = build_watch_mask(&watch_config.events);

        match inotify.watches().add(path, mask) {
            Ok(wd) => {
                wd_map.insert(wd.get_watch_descriptor_id(), i);
                eprintln!(
                    "phyl-listen: [{}] watching {}",
                    watch_config.name, watch_config.path
                );

                // Add recursive watches for subdirectories
                if watch_config.recursive && path.is_dir() {
                    add_recursive_watches(&mut inotify, path, mask, i, &mut wd_map);
                }
            }
            Err(e) => {
                eprintln!(
                    "phyl-listen: [{}] failed to watch {}: {e}",
                    watch_config.name, watch_config.path
                );
            }
        }
    }

    if wd_map.is_empty() {
        return Err("no watches could be established".to_string());
    }

    // Debounce state: path -> (last event time, event type, watch config index)
    let mut debounce_state: HashMap<PathBuf, (Instant, String, usize)> = HashMap::new();
    let mut buffer = vec![0u8; 4096];

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

            // Skip "create then delete" (no session)
            if event_type == "delete" {
                // Check if this was preceded by a create — if so, skip entirely
                // (simplified: we just fire the delete event)
            }

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

        // Poll for inotify events with a short timeout (for debounce checking)
        tokio::select! {
            result = tokio::task::spawn_blocking({
                let raw_fd = inotify.as_raw_fd();
                move || {
                    // Use poll() to wait for inotify events with 500ms timeout
                    let mut pollfd = libc::pollfd {
                        fd: raw_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    unsafe {
                        libc::poll(&mut pollfd, 1, 500)
                    }
                }
            }) => {
                match result {
                    Ok(poll_result) if poll_result > 0 => {
                        // Read events
                        match inotify.read_events(&mut buffer) {
                            Ok(events) => {
                                for event in events {
                                    let wd_id = event.wd.get_watch_descriptor_id();
                                    let config_idx = match wd_map.get(&wd_id) {
                                        Some(idx) => *idx,
                                        None => continue,
                                    };
                                    let watch_config = &watches[config_idx];

                                    // Get filename
                                    let filename = match event.name {
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

                                    // Map inotify event to our event type
                                    let event_type = mask_to_event_type(&event.mask);
                                    if event_type.is_empty() {
                                        continue;
                                    }

                                    // Check against configured event types
                                    if !watch_config.events.is_empty()
                                        && !watch_config.events.contains(&event_type)
                                    {
                                        continue;
                                    }

                                    let full_path = Path::new(&watch_config.path).join(&filename);

                                    // Add to debounce state
                                    debounce_state.insert(
                                        full_path,
                                        (Instant::now(), event_type, config_idx),
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!("phyl-listen: inotify read error: {e}");
                            }
                        }
                    }
                    _ => {} // timeout or error, continue loop
                }
            }
            _ = shutdown.changed() => {
                eprintln!("phyl-listen: file watches stopped");
                return Ok(());
            }
        }
    }
}

fn build_watch_mask(events: &[String]) -> WatchMask {
    let mut mask = WatchMask::empty();
    if events.is_empty() {
        // Watch everything by default
        return WatchMask::CREATE
            | WatchMask::MODIFY
            | WatchMask::DELETE
            | WatchMask::MOVED_TO
            | WatchMask::MOVED_FROM;
    }
    for event in events {
        match event.as_str() {
            "create" => mask |= WatchMask::CREATE,
            "modify" => mask |= WatchMask::MODIFY,
            "delete" => mask |= WatchMask::DELETE,
            "move_to" => mask |= WatchMask::MOVED_TO,
            "move_from" => mask |= WatchMask::MOVED_FROM,
            _ => eprintln!("phyl-listen: unknown watch event type: {event}"),
        }
    }
    mask
}

fn mask_to_event_type(mask: &EventMask) -> String {
    if mask.contains(EventMask::CREATE) {
        "create".to_string()
    } else if mask.contains(EventMask::MODIFY) {
        "modify".to_string()
    } else if mask.contains(EventMask::DELETE) {
        "delete".to_string()
    } else if mask.contains(EventMask::MOVED_TO) {
        "move_to".to_string()
    } else if mask.contains(EventMask::MOVED_FROM) {
        "move_from".to_string()
    } else {
        String::new()
    }
}

fn glob_matches(pattern: &str, filename: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| p.matches(filename))
        .unwrap_or(false)
}

fn add_recursive_watches(
    inotify: &mut Inotify,
    dir: &Path,
    mask: WatchMask,
    config_idx: usize,
    wd_map: &mut HashMap<i32, usize>,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if let Ok(wd) = inotify.watches().add(&path, mask) {
                    wd_map.insert(wd.get_watch_descriptor_id(), config_idx);
                }
                add_recursive_watches(inotify, &path, mask, config_idx, wd_map);
            }
        }
    }
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
    fn test_build_watch_mask_empty() {
        let mask = build_watch_mask(&[]);
        assert!(mask.contains(WatchMask::CREATE));
        assert!(mask.contains(WatchMask::MODIFY));
        assert!(mask.contains(WatchMask::DELETE));
    }

    #[test]
    fn test_build_watch_mask_specific() {
        let mask = build_watch_mask(&["create".to_string(), "modify".to_string()]);
        assert!(mask.contains(WatchMask::CREATE));
        assert!(mask.contains(WatchMask::MODIFY));
        assert!(!mask.contains(WatchMask::DELETE));
    }

    #[test]
    fn test_mask_to_event_type() {
        assert_eq!(mask_to_event_type(&EventMask::CREATE), "create");
        assert_eq!(mask_to_event_type(&EventMask::MODIFY), "modify");
        assert_eq!(mask_to_event_type(&EventMask::DELETE), "delete");
        assert_eq!(mask_to_event_type(&EventMask::MOVED_TO), "move_to");
        assert_eq!(mask_to_event_type(&EventMask::MOVED_FROM), "move_from");
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
