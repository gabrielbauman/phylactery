//! `phyl config` subcommands — configuration management.

use anyhow::{Context, bail};
use phyl_core::{Config, home_dir};
use std::path::{Path, PathBuf};

/// Run `phyl config <subcommand> [args...]`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        bail!("Usage: phyl config <show|validate|edit|add|add-secret|list-secrets|remove-secret>");
    }

    match args[0].as_str() {
        "show" => cmd_show(),
        "validate" => cmd_validate(),
        "edit" => cmd_edit(),
        "add" => cmd_add(&args[1..]),
        "add-secret" => cmd_add_secret(&args[1..]),
        "list-secrets" => cmd_list_secrets(),
        "remove-secret" => cmd_remove_secret(&args[1..]),
        other => bail!("unknown config subcommand: {other}"),
    }
}

/// Pretty-print resolved config with secrets masked.
fn cmd_show() -> anyhow::Result<()> {
    let home = home_dir();
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path).context("failed to read config.toml")?;

    // Load secrets for masking
    let secrets = load_secrets(&home);

    // Print config with secrets masked
    let mut output = contents.clone();
    for (_key, value) in &secrets {
        if value.len() > 4 {
            let masked = format!(
                "{}{}",
                &value[..3],
                "\u{2022}".repeat(value.len().min(10) - 3)
            );
            output = output.replace(value, &masked);
        }
    }

    println!("{output}");
    Ok(())
}

/// Validate config.toml for errors.
fn cmd_validate() -> anyhow::Result<()> {
    let home = home_dir();
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path).context("failed to read config.toml")?;

    let config: Config = toml::from_str(&contents).context("config.toml parse error")?;

    let mut warnings = Vec::new();

    // Check model adapter exists
    if which(&config.session.model).is_none() {
        warnings.push(format!(
            "model adapter '{}' not found on $PATH",
            config.session.model
        ));
    }

    // Check poll commands exist
    for rule in &config.poll {
        if !rule.shell && which(&rule.command).is_none() {
            warnings.push(format!(
                "poll rule '{}': command '{}' not found on $PATH",
                rule.name, rule.command
            ));
        }
    }

    // Check for duplicate poll names
    let mut poll_names = std::collections::HashSet::new();
    for rule in &config.poll {
        if !poll_names.insert(&rule.name) {
            warnings.push(format!("duplicate poll name: '{}'", rule.name));
        }
    }

    // Check for duplicate hook names
    if let Some(listen) = &config.listen {
        let mut hook_names = std::collections::HashSet::new();
        for hook in &listen.hook {
            if !hook_names.insert(&hook.name) {
                warnings.push(format!("duplicate hook name: '{}'", hook.name));
            }
        }

        let mut watch_names = std::collections::HashSet::new();
        for watch in &listen.watch {
            if !watch_names.insert(&watch.name) {
                warnings.push(format!("duplicate watch name: '{}'", watch.name));
            }
        }
    }

    // Check secret references
    let secrets = load_secrets(&home);
    check_secret_refs(&contents, &secrets, &mut warnings);

    if warnings.is_empty() {
        eprintln!("config.toml: valid, no warnings");
    } else {
        eprintln!("config.toml: valid with {} warning(s):", warnings.len());
        for w in &warnings {
            eprintln!("  - {w}");
        }
    }

    Ok(())
}

/// Open config.toml in $EDITOR.
fn cmd_edit() -> anyhow::Result<()> {
    let home = home_dir();
    let config_path = home.join("config.toml");

    let editor = std::env::var("EDITOR")
        .unwrap_or_else(|_| std::env::var("VISUAL").unwrap_or_else(|_| "vi".to_string()));

    let status = std::process::Command::new(&editor)
        .arg(&config_path)
        .status()
        .with_context(|| format!("failed to launch editor '{editor}'"))?;

    if !status.success() {
        bail!("editor exited with error");
    }

    // Validate after editing
    eprintln!("Validating config...");
    cmd_validate()
}

/// Add a config section interactively or via flags.
fn cmd_add(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        bail!("Usage: phyl config add <mcp|poll|hook|sse|watch|bridge> <name>");
    }

    let section_type = args[0].as_str();
    let name = args.get(1).map(|s| s.as_str()).unwrap_or("");

    let home = home_dir();
    let config_path = home.join("config.toml");

    let snippet = match section_type {
        "mcp" => {
            if name.is_empty() {
                bail!("Usage: phyl config add mcp <name>");
            }
            format!(
                r#"
[[mcp]]
name = "{name}"
command = ""          # Path to MCP server command
# args = []
# env = {{ }}
"#
            )
        }
        "poll" => {
            if name.is_empty() {
                bail!("Usage: phyl config add poll <name>");
            }
            format!(
                r#"
[[poll]]
name = "{name}"
command = ""          # Command to run
# args = []
interval = 300        # Seconds between polls
prompt = ""           # What to do when output changes
# shell = false       # Run via sh -c
# timeout = 30        # Command timeout in seconds
"#
            )
        }
        "hook" => {
            if name.is_empty() {
                bail!("Usage: phyl config add hook <name>");
            }
            format!(
                r#"
[[listen.hook]]
name = "{name}"
path = "/hook/{name}"
prompt = ""           # What to do when webhook fires
# secret = "$SECRET"  # HMAC-SHA256 secret (env var reference)
# route_header = "X-GitHub-Event"
# routes = {{ push = "Code pushed", pull_request = "PR opened" }}
"#
            )
        }
        "sse" => {
            if name.is_empty() {
                bail!("Usage: phyl config add sse <name>");
            }
            format!(
                r#"
[[listen.sse]]
name = "{name}"
url = ""              # SSE endpoint URL
prompt = ""           # What to do when event arrives
# headers = {{ Authorization = "Bearer $TOKEN" }}
# events = []         # Filter to specific event types
# route_event = false
# routes = {{ }}
"#
            )
        }
        "watch" => {
            if name.is_empty() {
                bail!("Usage: phyl config add watch <name>");
            }
            format!(
                r#"
[[listen.watch]]
name = "{name}"
path = ""             # Directory or file to watch
prompt = ""           # What to do when file changes
# events = ["create", "modify"]
# recursive = false
# glob = "*.txt"      # Only match these files
# debounce = 2        # Seconds to coalesce events
"#
            )
        }
        "bridge" => {
            if args.get(1).map(|s| s.as_str()) != Some("signal") {
                bail!("Usage: phyl config add bridge signal");
            }
            r#"
[bridge.signal]
phone = ""            # Agent's registered Signal number
owner = ""            # Your Signal number (only accept from this)
signal_cli = "signal-cli"
"#
            .to_string()
        }
        other => {
            bail!("unknown config section: {other}. Use: mcp, poll, hook, sse, watch, bridge");
        }
    };

    // Append to config.toml
    let mut contents =
        std::fs::read_to_string(&config_path).context("failed to read config.toml")?;
    contents.push_str(&snippet);
    std::fs::write(&config_path, &contents).context("failed to write config.toml")?;

    eprintln!("Added {section_type} section to config.toml");
    eprintln!("Edit with: phyl config edit");
    Ok(())
}

/// Add a secret to secrets.env.
fn cmd_add_secret(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 2 {
        bail!("Usage: phyl config add-secret <KEY> <VALUE>");
    }

    let key = &args[0];
    let value = &args[1];
    let home = home_dir();
    let secrets_path = home.join("secrets.env");

    // Read existing secrets
    let mut contents = std::fs::read_to_string(&secrets_path).unwrap_or_default();

    // Check if key already exists
    for line in contents.lines() {
        let line = line.trim();
        if let Some((k, _)) = line.split_once('=')
            && k.trim() == key
        {
            bail!(
                "secret '{key}' already exists. Remove it first with: phyl config remove-secret {key}"
            );
        }
    }

    // Append
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&format!("{key}={value}\n"));
    std::fs::write(&secrets_path, &contents).context("failed to write secrets.env")?;

    eprintln!("Added secret: {key}");
    Ok(())
}

/// List secret keys with values masked.
fn cmd_list_secrets() -> anyhow::Result<()> {
    let home = home_dir();
    let secrets = load_secrets(&home);

    if secrets.is_empty() {
        eprintln!("No secrets configured.");
        return Ok(());
    }

    for (key, value) in &secrets {
        let masked = if value.len() > 4 {
            format!(
                "{}{}",
                &value[..3],
                "\u{2022}".repeat(value.len().min(10) - 3)
            )
        } else {
            "\u{2022}".repeat(value.len())
        };
        println!("{key}={masked}");
    }
    Ok(())
}

/// Remove a secret from secrets.env.
fn cmd_remove_secret(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        bail!("Usage: phyl config remove-secret <KEY>");
    }

    let key = &args[0];
    let home = home_dir();
    let secrets_path = home.join("secrets.env");
    let contents = std::fs::read_to_string(&secrets_path).context("failed to read secrets.env")?;

    let mut found = false;
    let mut new_contents = String::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == key
        {
            found = true;
            continue;
        }
        new_contents.push_str(line);
        new_contents.push('\n');
    }

    if !found {
        bail!("secret '{key}' not found");
    }

    std::fs::write(&secrets_path, &new_contents).context("failed to write secrets.env")?;

    eprintln!("Removed secret: {key}");
    Ok(())
}

/// Load secrets from secrets.env as key-value pairs.
fn load_secrets(home: &Path) -> Vec<(String, String)> {
    let path = home.join("secrets.env");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut secrets = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            secrets.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    secrets
}

/// Check for $VAR references in config that don't have matching secrets.
fn check_secret_refs(config_text: &str, secrets: &[(String, String)], warnings: &mut Vec<String>) {
    let secret_keys: std::collections::HashSet<&str> =
        secrets.iter().map(|(k, _)| k.as_str()).collect();

    // Find $VAR patterns in the config text (skip comments)
    for line in config_text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let mut i = 0;
        let bytes = line.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let start = if bytes[i + 1] == b'{' { i + 2 } else { i + 1 };
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    let var_name = &line[start..end];
                    // Skip well-known non-secret vars
                    if !matches!(
                        var_name,
                        "XDG_RUNTIME_DIR"
                            | "HOME"
                            | "PATH"
                            | "EDITOR"
                            | "VISUAL"
                            | "PHYLACTERY_HOME"
                    ) && !secret_keys.contains(var_name)
                        && std::env::var(var_name).is_err()
                    {
                        warnings.push(format!(
                            "config references ${var_name} but it's not in secrets.env or environment"
                        ));
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
    }
}

/// Check if a command exists on $PATH.
fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok().and_then(|path| {
        for dir in path.split(':') {
            let candidate = Path::new(dir).join(cmd);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_secrets_empty() {
        let dir = std::env::temp_dir().join("phyl-config-test-empty");
        let _ = std::fs::create_dir_all(&dir);
        let secrets = load_secrets(&dir);
        assert!(secrets.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_secrets_with_entries() {
        let dir = std::env::temp_dir().join("phyl-config-test-entries");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("secrets.env"),
            "# comment\nKEY_A=value_a\n\nKEY_B=value_b\n",
        )
        .unwrap();
        let secrets = load_secrets(&dir);
        assert_eq!(secrets.len(), 2);
        assert_eq!(secrets[0], ("KEY_A".to_string(), "value_a".to_string()));
        assert_eq!(secrets[1], ("KEY_B".to_string(), "value_b".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_check_secret_refs_missing() {
        let config = r#"
[bridge.signal]
phone = "+1234567890"
secret = "$MISSING_SECRET"
"#;
        let secrets = vec![("KNOWN_KEY".to_string(), "value".to_string())];
        let mut warnings = Vec::new();
        check_secret_refs(config, &secrets, &mut warnings);
        assert!(
            warnings.iter().any(|w| w.contains("MISSING_SECRET")),
            "expected warning about MISSING_SECRET: {warnings:?}"
        );
    }

    #[test]
    fn test_check_secret_refs_present() {
        let config = r#"secret = "$MY_KEY""#;
        let secrets = vec![("MY_KEY".to_string(), "value".to_string())];
        let mut warnings = Vec::new();
        check_secret_refs(config, &secrets, &mut warnings);
        assert!(warnings.is_empty(), "expected no warnings: {warnings:?}");
    }

    #[test]
    fn test_check_secret_refs_skip_comments() {
        let config = r#"# secret = "$COMMENTED_SECRET""#;
        let secrets = vec![];
        let mut warnings = Vec::new();
        check_secret_refs(config, &secrets, &mut warnings);
        assert!(warnings.is_empty());
    }
}
