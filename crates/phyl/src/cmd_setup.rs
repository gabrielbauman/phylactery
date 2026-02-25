//! `phyl setup` subcommands — service management and system setup.
//!
//! Platform-aware: uses systemd on Linux and launchd on macOS.

use anyhow::{Context, bail};
use phyl_core::{Config, home_dir};
use std::path::{Path, PathBuf};

/// Run `phyl setup <subcommand>`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        let services_cmd = if cfg!(target_os = "macos") {
            "launchd"
        } else {
            "systemd"
        };
        bail!("Usage: phyl setup <{services_cmd}|status|migrate-xdg>");
    }

    match args[0].as_str() {
        "systemd" => cmd_install_services_systemd(),
        "launchd" => cmd_install_services_launchd(),
        "services" => cmd_install_services(),
        "status" => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(cmd_status())
        }
        "migrate-xdg" => {
            let force = args.iter().any(|a| a == "--force");
            cmd_migrate_xdg(force)
        }
        other => bail!("unknown setup subcommand: {other}"),
    }
}

/// Auto-detect platform and install the appropriate service definitions.
fn cmd_install_services() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        cmd_install_services_launchd()
    } else {
        cmd_install_services_systemd()
    }
}

// ---------------------------------------------------------------------------
// systemd (Linux)
// ---------------------------------------------------------------------------

/// Generate, install, and enable systemd user units.
fn cmd_install_services_systemd() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        eprintln!("Note: systemd is not available on macOS. Use `phyl setup launchd` instead.");
        bail!("systemd is not available on this platform");
    }

    let home = home_dir();
    let config = load_config(&home)?;
    let home_str = home.to_string_lossy();

    let systemd_dir = dirs_config_home().join("systemd/user");
    std::fs::create_dir_all(&systemd_dir).context("failed to create systemd dir")?;

    let bin_dir = find_bin_dir();

    // Always generate phylactd.service
    let unit = generate_systemd_unit(
        "Phylactery daemon",
        &format!("{bin_dir}/phylactd"),
        &home_str,
        None, // no dependency
    );
    write_file_to(&systemd_dir, "phylactd.service", &unit)?;
    eprintln!("  wrote phylactd.service");

    // Generate phyl-poll.service if poll rules exist
    if !config.poll.is_empty() {
        let unit = generate_systemd_unit(
            "Phylactery poller",
            &format!("{bin_dir}/phyl-poll"),
            &home_str,
            Some("phylactd.service"),
        );
        write_file_to(&systemd_dir, "phyl-poll.service", &unit)?;
        eprintln!("  wrote phyl-poll.service");
    }

    // Generate phyl-listen.service if listen config exists
    if let Some(listen) = &config.listen
        && (!listen.hook.is_empty() || !listen.sse.is_empty() || !listen.watch.is_empty())
    {
        let unit = generate_systemd_unit(
            "Phylactery listener",
            &format!("{bin_dir}/phyl-listen"),
            &home_str,
            Some("phylactd.service"),
        );
        write_file_to(&systemd_dir, "phyl-listen.service", &unit)?;
        eprintln!("  wrote phyl-listen.service");
    }

    // Generate phyl-bridge-signal.service if bridge configured
    if let Some(bridge) = &config.bridge
        && bridge.signal.is_some()
    {
        let unit = generate_systemd_unit(
            "Phylactery Signal bridge",
            &format!("{bin_dir}/phyl-bridge-signal"),
            &home_str,
            Some("phylactd.service"),
        );
        write_file_to(&systemd_dir, "phyl-bridge-signal.service", &unit)?;
        eprintln!("  wrote phyl-bridge-signal.service");
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

fn generate_systemd_unit(
    description: &str,
    exec_start: &str,
    home: &str,
    after: Option<&str>,
) -> String {
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

// ---------------------------------------------------------------------------
// launchd (macOS)
// ---------------------------------------------------------------------------

/// Generate, install, and load launchd user agents.
fn cmd_install_services_launchd() -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!("Note: launchd is only available on macOS. Use `phyl setup systemd` instead.");
        bail!("launchd is not available on this platform");
    }

    let home = home_dir();
    let config = load_config(&home)?;
    let home_str = home.to_string_lossy().to_string();

    let agents_dir = launchd_agents_dir()?;
    std::fs::create_dir_all(&agents_dir).context("failed to create LaunchAgents dir")?;

    let bin_dir = find_bin_dir();

    // Load secrets for EnvironmentVariables in plists
    let secrets = load_secrets_map(&home);

    // Always generate phylactd plist
    let plist = generate_launchd_plist(
        "com.phylactery.daemon",
        &format!("{bin_dir}/phylactd"),
        &home_str,
        &secrets,
        true, // KeepAlive
    );
    write_file_to(&agents_dir, "com.phylactery.daemon.plist", &plist)?;
    eprintln!("  wrote com.phylactery.daemon.plist");

    // Generate phyl-poll plist if poll rules exist
    if !config.poll.is_empty() {
        let plist = generate_launchd_plist(
            "com.phylactery.poll",
            &format!("{bin_dir}/phyl-poll"),
            &home_str,
            &secrets,
            true,
        );
        write_file_to(&agents_dir, "com.phylactery.poll.plist", &plist)?;
        eprintln!("  wrote com.phylactery.poll.plist");
    }

    // Generate phyl-listen plist if listen config exists
    if let Some(listen) = &config.listen
        && (!listen.hook.is_empty() || !listen.sse.is_empty() || !listen.watch.is_empty())
    {
        let plist = generate_launchd_plist(
            "com.phylactery.listen",
            &format!("{bin_dir}/phyl-listen"),
            &home_str,
            &secrets,
            true,
        );
        write_file_to(&agents_dir, "com.phylactery.listen.plist", &plist)?;
        eprintln!("  wrote com.phylactery.listen.plist");
    }

    // Generate phyl-bridge-signal plist if bridge configured
    if let Some(bridge) = &config.bridge
        && bridge.signal.is_some()
    {
        let plist = generate_launchd_plist(
            "com.phylactery.bridge-signal",
            &format!("{bin_dir}/phyl-bridge-signal"),
            &home_str,
            &secrets,
            true,
        );
        write_file_to(&agents_dir, "com.phylactery.bridge-signal.plist", &plist)?;
        eprintln!("  wrote com.phylactery.bridge-signal.plist");
    }

    // Load all phylactery agents
    eprintln!("Loading launch agents...");
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("com.phylactery.") && name.ends_with(".plist") {
            let path = entry.path();
            // Unload first (ignore errors if not loaded)
            let _ = run_cmd("launchctl", &["unload", &path.to_string_lossy()]);
            run_cmd("launchctl", &["load", &path.to_string_lossy()])?;
            eprintln!("  loaded {name}");
        }
    }

    eprintln!("Done. Check status with: phyl setup status");
    Ok(())
}

fn generate_launchd_plist(
    label: &str,
    program: &str,
    home: &str,
    secrets: &[(String, String)],
    keep_alive: bool,
) -> String {
    let mut env_entries =
        format!("      <key>PHYLACTERY_HOME</key>\n      <string>{home}</string>");
    for (key, value) in secrets {
        env_entries.push_str(&format!(
            "\n      <key>{key}</key>\n      <string>{value}</string>"
        ));
    }

    let log_dir = format!("{home}/logs");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env_entries}
    </dict>
    <key>KeepAlive</key>
    <{keep_alive}/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/{label}.out.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/{label}.err.log</string>
</dict>
</plist>
"#
    )
}

fn launchd_agents_dir() -> anyhow::Result<PathBuf> {
    let home = home_env().context("HOME not set")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents"))
}

fn load_secrets_map(home: &Path) -> Vec<(String, String)> {
    let path = home.join("secrets.env");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Status (cross-platform)
// ---------------------------------------------------------------------------

/// Show health of all components.
async fn cmd_status() -> anyhow::Result<()> {
    let home = home_dir();
    let config = load_config(&home)?;

    println!("Phylactery Status");
    println!("{}", "\u{2500}".repeat(17));

    // Home directory
    let home_str = home.to_string_lossy();
    let home_type = if home_str.contains("Application Support") {
        "macOS"
    } else if home_str.contains(".local/share") {
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
        check_service_status("phyl-poll", "com.phylactery.poll")
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
                check_service_status("phyl-listen", "com.phylactery.listen"),
                l.hook.len(),
                l.sse.len(),
                l.watch.len()
            )
        }
        _ => "not configured".to_string(),
    };
    println!("  phyl-listen       {listen_status}");

    let signal_status = match &config.bridge {
        Some(b) if b.signal.is_some() => {
            check_service_status("phyl-bridge-signal", "com.phylactery.bridge-signal")
        }
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

// ---------------------------------------------------------------------------
// migrate-xdg
// ---------------------------------------------------------------------------

/// Migrate ~/.phylactery to platform-appropriate data directory.
fn cmd_migrate_xdg(force: bool) -> anyhow::Result<()> {
    let legacy_home = home_env()
        .map(|h| PathBuf::from(h).join(".phylactery"))
        .context("cannot determine home directory")?;

    let new_home = platform_data_home()?;

    if !legacy_home.exists() {
        bail!("{} does not exist", legacy_home.display());
    }

    if new_home.exists() {
        bail!("{} already exists", new_home.display());
    }

    if !force {
        eprintln!(
            "This will move {} to {}",
            legacy_home.display(),
            new_home.display()
        );
        eprintln!("Make sure all services are stopped. Use --force to proceed.");
        return Ok(());
    }

    // Create parent directory
    if let Some(parent) = new_home.parent() {
        std::fs::create_dir_all(parent).context("failed to create directory")?;
    }

    // Move directory
    std::fs::rename(&legacy_home, &new_home).context("failed to move directory")?;
    eprintln!("Moved {} -> {}", legacy_home.display(), new_home.display());

    // Create config symlink (XDG convention, skip on macOS)
    #[cfg(not(target_os = "macos"))]
    {
        let config_dir = home_env()
            .map(|h| PathBuf::from(h).join(".config/phylactery"))
            .context("cannot determine config directory")?;
        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            eprintln!(
                "Warning: failed to create config directory {}: {e}",
                config_dir.display()
            );
        }
        let config_link = config_dir.join("config.toml");
        let config_target = new_home.join("config.toml");
        if let Err(e) = std::os::unix::fs::symlink(&config_target, &config_link) {
            eprintln!("Warning: failed to create config symlink: {e}");
        } else {
            eprintln!(
                "Created symlink {} -> {}",
                config_link.display(),
                config_target.display()
            );
        }
    }

    eprintln!();
    eprintln!(
        "Migration complete. Set PHYLACTERY_HOME={}",
        new_home.display()
    );
    let services_cmd = if cfg!(target_os = "macos") {
        "launchd"
    } else {
        "systemd"
    };
    eprintln!("Or re-run: phyl setup {services_cmd}");

    Ok(())
}

/// Returns the platform-appropriate data directory for migration.
fn platform_data_home() -> anyhow::Result<PathBuf> {
    let home = home_env().context("cannot determine home directory")?;

    if cfg!(target_os = "macos") {
        Ok(PathBuf::from(&home).join("Library/Application Support/phylactery"))
    } else {
        Ok(PathBuf::from(&home).join(".local/share/phylactery"))
    }
}

// --- Helper functions ---

fn write_file_to(dir: &Path, name: &str, content: &str) -> anyhow::Result<()> {
    std::fs::write(dir.join(name), content).with_context(|| format!("failed to write {name}"))
}

fn run_cmd(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {cmd}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "  warning: {cmd} {} failed: {}",
            args.join(" "),
            stderr.trim()
        );
    }
    Ok(())
}

fn load_config(home: &Path) -> anyhow::Result<Config> {
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path).context("failed to read config.toml")?;
    toml::from_str(&contents).context("failed to parse config.toml")
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

/// Check service status, using systemd on Linux or launchd on macOS.
fn check_service_status(systemd_name: &str, launchd_label: &str) -> String {
    if cfg!(target_os = "macos") {
        check_launchd_status(launchd_label)
    } else {
        check_systemd_status(systemd_name)
    }
}

fn check_systemd_status(service: &str) -> String {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "is-active", &format!("{service}.service")])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "stopped".to_string(),
    }
}

fn check_launchd_status(label: &str) -> String {
    let output = std::process::Command::new("launchctl")
        .args(["list", label])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            // Parse the output — launchctl list <label> prints a table with PID
            let stdout = String::from_utf8_lossy(&o.stdout);
            if stdout.contains("\"PID\"") || stdout.lines().any(|l| l.contains("PID")) {
                "active".to_string()
            } else {
                "loaded".to_string()
            }
        }
        _ => "stopped".to_string(),
    }
}

async fn get_session_summary(socket: &str) -> anyhow::Result<String> {
    let (status, body) = crate::client::get(socket, "/sessions")
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if !status.is_success() {
        bail!("failed to get sessions");
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
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        return dir.to_string_lossy().to_string();
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
    fn test_generate_systemd_unit_daemon() {
        let unit = generate_systemd_unit(
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
    fn test_generate_systemd_unit_with_dependency() {
        let unit = generate_systemd_unit(
            "Phylactery poller",
            "/usr/local/bin/phyl-poll",
            "/home/user/.local/share/phylactery",
            Some("phylactd.service"),
        );
        assert!(unit.contains("After=phylactd.service"));
        assert!(unit.contains("Requires=phylactd.service"));
    }

    #[test]
    fn test_generate_launchd_plist() {
        let plist = generate_launchd_plist(
            "com.phylactery.daemon",
            "/usr/local/bin/phylactd",
            "/home/user/Library/Application Support/phylactery",
            &[],
            true,
        );
        assert!(plist.contains("<string>com.phylactery.daemon</string>"));
        assert!(plist.contains("<string>/usr/local/bin/phylactd</string>"));
        assert!(plist.contains("<key>PHYLACTERY_HOME</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
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
