//! `phyl setup` subcommands — service management and system setup.

use phyl_core::{Config, home_dir};
use std::path::{Path, PathBuf};

/// Run `phyl setup <subcommand>`.
pub fn run(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: phyl setup <systemd|status|migrate-xdg>".to_string());
    }

    match args[0].as_str() {
        "systemd" => cmd_systemd(),
        "status" => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(cmd_status())
        }
        "migrate-xdg" => {
            let force = args.iter().any(|a| a == "--force");
            cmd_migrate_xdg(force)
        }
        other => Err(format!("unknown setup subcommand: {other}")),
    }
}

/// Generate, install, and enable systemd user units.
fn cmd_systemd() -> Result<(), String> {
    let home = home_dir();
    let config = load_config(&home)?;
    let home_str = home.to_string_lossy();

    let systemd_dir = dirs_config_home().join("systemd/user");
    std::fs::create_dir_all(&systemd_dir)
        .map_err(|e| format!("failed to create systemd dir: {e}"))?;

    let bin_dir = find_bin_dir();

    // Always generate phylactd.service
    let unit = generate_unit(
        "Phylactery daemon",
        &format!("{bin_dir}/phylactd"),
        &home_str,
        None, // no dependency
    );
    write_unit(&systemd_dir, "phylactd.service", &unit)?;
    eprintln!("  wrote phylactd.service");

    // Generate phyl-poll.service if poll rules exist
    if !config.poll.is_empty() {
        let unit = generate_unit(
            "Phylactery poller",
            &format!("{bin_dir}/phyl-poll"),
            &home_str,
            Some("phylactd.service"),
        );
        write_unit(&systemd_dir, "phyl-poll.service", &unit)?;
        eprintln!("  wrote phyl-poll.service");
    }

    // Generate phyl-listen.service if listen config exists
    if let Some(listen) = &config.listen {
        if !listen.hook.is_empty() || !listen.sse.is_empty() || !listen.watch.is_empty() {
            let unit = generate_unit(
                "Phylactery listener",
                &format!("{bin_dir}/phyl-listen"),
                &home_str,
                Some("phylactd.service"),
            );
            write_unit(&systemd_dir, "phyl-listen.service", &unit)?;
            eprintln!("  wrote phyl-listen.service");
        }
    }

    // Generate phyl-bridge-signal.service if bridge configured
    if let Some(bridge) = &config.bridge {
        if bridge.signal.is_some() {
            let unit = generate_unit(
                "Phylactery Signal bridge",
                &format!("{bin_dir}/phyl-bridge-signal"),
                &home_str,
                Some("phylactd.service"),
            );
            write_unit(&systemd_dir, "phyl-bridge-signal.service", &unit)?;
            eprintln!("  wrote phyl-bridge-signal.service");
        }
    }

    // Reload systemd
    eprintln!("Reloading systemd...");
    run_cmd("systemctl", &["--user", "daemon-reload"])?;

    // Enable and start the daemon
    run_cmd("systemctl", &["--user", "enable", "phylactd.service"])?;
    run_cmd("systemctl", &["--user", "start", "phylactd.service"])?;
    eprintln!("  enabled + started phylactd");

    // Enable and start services if their units exist
    for service in &["phyl-poll", "phyl-listen", "phyl-bridge-signal"] {
        let unit_file = systemd_dir.join(format!("{service}.service"));
        if unit_file.exists() {
            let svc = format!("{service}.service");
            run_cmd("systemctl", &["--user", "enable", &svc])?;
            run_cmd("systemctl", &["--user", "start", &svc])?;
            eprintln!("  enabled + started {service}");
        }
    }

    eprintln!("Done. Check status with: phyl setup status");
    Ok(())
}

/// Show health of all components.
async fn cmd_status() -> Result<(), String> {
    let home = home_dir();
    let config = load_config(&home)?;

    println!("Phylactery Status");
    println!("{}", "\u{2500}".repeat(17));

    // Home directory
    let home_type = if home.to_string_lossy().contains(".local/share") {
        "XDG"
    } else {
        "legacy"
    };
    println!("  Home:     {} ({home_type})", home.display());

    // Config
    let config_status = match toml::from_str::<Config>(
        &std::fs::read_to_string(home.join("config.toml")).unwrap_or_default(),
    ) {
        Ok(_) => "valid".to_string(),
        Err(e) => format!("error: {e}"),
    };
    println!("  Config:   config.toml ({config_status})");

    // Secrets
    let secrets_count = count_secrets(&home);
    println!("  Secrets:  secrets.env ({secrets_count} keys)");

    println!();
    println!("Services");
    println!("{}", "\u{2500}".repeat(8));

    // Check daemon
    let socket = &config.daemon.socket;
    let daemon_status = check_daemon_status(socket).await;
    println!("  phylactd          {daemon_status}");

    // Check other services
    let poll_status = if config.poll.is_empty() {
        "not configured".to_string()
    } else {
        check_service_status("phyl-poll")
    };
    println!(
        "  phyl-poll         {poll_status}{}",
        if !config.poll.is_empty() {
            format!(" ({} rule(s))", config.poll.len())
        } else {
            String::new()
        }
    );

    let listen_status = match &config.listen {
        Some(l) if !l.hook.is_empty() || !l.sse.is_empty() || !l.watch.is_empty() => {
            format!(
                "{} ({} hook(s), {} SSE, {} watch(es))",
                check_service_status("phyl-listen"),
                l.hook.len(),
                l.sse.len(),
                l.watch.len()
            )
        }
        _ => "not configured".to_string(),
    };
    println!("  phyl-listen       {listen_status}");

    let signal_status = match &config.bridge {
        Some(b) if b.signal.is_some() => check_service_status("phyl-bridge-signal"),
        _ => "not configured".to_string(),
    };
    println!("  phyl-bridge-signal {signal_status}");

    // Session summary (if daemon is running)
    if daemon_status.starts_with("running") {
        println!();
        println!("Sessions");
        println!("{}", "\u{2500}".repeat(8));
        match get_session_summary(socket).await {
            Ok(summary) => println!("  {summary}"),
            Err(_) => println!("  (could not retrieve)"),
        }
    }

    Ok(())
}

/// Migrate ~/.phylactery to XDG paths.
fn cmd_migrate_xdg(force: bool) -> Result<(), String> {
    let legacy_home = home_env()
        .map(|h| PathBuf::from(h).join(".phylactery"))
        .ok_or("cannot determine home directory")?;

    let xdg_home = home_env()
        .map(|h| PathBuf::from(h).join(".local/share/phylactery"))
        .ok_or("cannot determine home directory")?;

    if !legacy_home.exists() {
        return Err(format!("{} does not exist", legacy_home.display()));
    }

    if xdg_home.exists() {
        return Err(format!("{} already exists", xdg_home.display()));
    }

    if !force {
        eprintln!("This will move {} to {}", legacy_home.display(), xdg_home.display());
        eprintln!("Make sure all services are stopped. Use --force to proceed.");
        return Ok(());
    }

    // Create parent directory
    if let Some(parent) = xdg_home.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory: {e}"))?;
    }

    // Move directory
    std::fs::rename(&legacy_home, &xdg_home)
        .map_err(|e| format!("failed to move directory: {e}"))?;
    eprintln!("Moved {} -> {}", legacy_home.display(), xdg_home.display());

    // Create config symlink
    let config_dir = home_env()
        .map(|h| PathBuf::from(h).join(".config/phylactery"))
        .ok_or("cannot determine config directory")?;
    let _ = std::fs::create_dir_all(&config_dir);
    let config_link = config_dir.join("config.toml");
    let config_target = xdg_home.join("config.toml");
    let _ = std::os::unix::fs::symlink(&config_target, &config_link);
    eprintln!("Created symlink {} -> {}", config_link.display(), config_target.display());

    eprintln!();
    eprintln!("Migration complete. Set PHYLACTERY_HOME={}", xdg_home.display());
    eprintln!("Or re-run: phyl setup systemd");

    Ok(())
}

// --- Helper functions ---

fn generate_unit(description: &str, exec_start: &str, home: &str, after: Option<&str>) -> String {
    let after_line = match after {
        Some(dep) => format!("After={dep}\nRequires={dep}"),
        None => "After=default.target".to_string(),
    };

    format!(
        r#"[Unit]
Description={description}
{after_line}

[Service]
Type=simple
ExecStart={exec_start}
Environment=PHYLACTERY_HOME={home}
EnvironmentFile={home}/secrets.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#
    )
}

fn write_unit(dir: &Path, name: &str, content: &str) -> Result<(), String> {
    std::fs::write(dir.join(name), content)
        .map_err(|e| format!("failed to write {name}: {e}"))
}

fn run_cmd(cmd: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run {cmd}: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("  warning: {cmd} {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn load_config(home: &Path) -> Result<Config, String> {
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.toml: {e}"))?;
    toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))
}

fn count_secrets(home: &Path) -> usize {
    let path = home.join("secrets.env");
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with('#') && l.contains('=')
        })
        .count()
}

async fn check_daemon_status(socket: &str) -> String {
    match crate::client::get(socket, "/health").await {
        Ok((status, body)) if status.is_success() => {
            format!("running ({})", body.trim())
        }
        _ => "stopped".to_string(),
    }
}

fn check_service_status(service: &str) -> String {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "is-active", &format!("{service}.service")])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => "stopped".to_string(),
    }
}

async fn get_session_summary(socket: &str) -> Result<String, String> {
    let (status, body) = crate::client::get(socket, "/sessions")
        .await
        .map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err("failed to get sessions".to_string());
    }

    let sessions: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();
    let active = sessions
        .iter()
        .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("running"))
        .count();
    let total = sessions.len();
    let done = sessions
        .iter()
        .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("done"))
        .count();
    let failed = sessions
        .iter()
        .filter(|s| {
            let s = s.get("status").and_then(|v| v.as_str()).unwrap_or("");
            s == "crashed" || s == "timed_out"
        })
        .count();

    Ok(format!(
        "active: {active}   completed: {done}   failed: {failed}   total: {total}"
    ))
}

fn find_bin_dir() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.to_string_lossy().to_string();
        }
    }
    // Fallback
    home_env()
        .map(|h| format!("{h}/.local/bin"))
        .unwrap_or_else(|| "/usr/local/bin".to_string())
}

fn dirs_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            home_env()
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
}

fn home_env() -> Option<String> {
    std::env::var("HOME").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_unit_daemon() {
        let unit = generate_unit(
            "Phylactery daemon",
            "/usr/local/bin/phylactd",
            "/home/user/.local/share/phylactery",
            None,
        );
        assert!(unit.contains("Description=Phylactery daemon"));
        assert!(unit.contains("ExecStart=/usr/local/bin/phylactd"));
        assert!(unit.contains("After=default.target"));
        assert!(unit.contains("EnvironmentFile="));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn test_generate_unit_with_dependency() {
        let unit = generate_unit(
            "Phylactery poller",
            "/usr/local/bin/phyl-poll",
            "/home/user/.local/share/phylactery",
            Some("phylactd.service"),
        );
        assert!(unit.contains("After=phylactd.service"));
        assert!(unit.contains("Requires=phylactd.service"));
    }

    #[test]
    fn test_count_secrets() {
        let dir = std::env::temp_dir().join("phyl-setup-test-secrets");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("secrets.env"),
            "# comment\nKEY_A=val\n\nKEY_B=val\n",
        )
        .unwrap();
        assert_eq!(count_secrets(&dir), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_count_secrets_empty() {
        let dir = std::env::temp_dir().join("phyl-setup-test-empty");
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(count_secrets(&dir), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
