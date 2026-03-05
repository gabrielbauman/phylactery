mod client;
mod cmd_config;
mod cmd_log;
mod cmd_ls;
mod cmd_say;
mod cmd_session;
mod cmd_setup;
mod cmd_start;
mod cmd_status;
mod cmd_stop;
mod cmd_watch;
mod format;
mod init;

use anyhow::{Context, bail};
use std::process;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("phyl: {e:#}");
        process::exit(1);
    }
}

fn try_main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "init" => {
            let path = args.get(2).map(|s| s.as_str());
            let systemd = args.iter().any(|a| a == "--systemd");
            init::run(path).context("init failed")?;
            if systemd {
                cmd_setup::run(&["systemd".to_string()]).context("setup systemd failed")?;
            }
        }
        "start" => {
            let detach = args.iter().any(|a| a == "-d");
            let all = args.iter().any(|a| a == "--all");
            if all {
                run_async(cmd_start::run_all())?;
            } else {
                cmd_start::run(detach).context("start failed")?;
            }
        }
        "session" => {
            // Parse: phyl session [-d] "prompt"
            let mut detach = false;
            let mut prompt: Option<&str> = None;
            for arg in &args[2..] {
                if arg == "-d" {
                    detach = true;
                } else if prompt.is_none() {
                    prompt = Some(arg.as_str());
                }
            }
            let prompt = match prompt {
                Some(p) => p,
                None => bail!("Usage: phyl session [-d] \"prompt\""),
            };
            run_async(cmd_session::run(prompt, detach))?;
        }
        "ls" => {
            run_async(cmd_ls::run())?;
        }
        "status" => {
            let id = args
                .get(2)
                .map(|s| s.as_str())
                .context("Usage: phyl status <session-id>")?;
            run_async(cmd_status::run(id))?;
        }
        "say" => {
            let id = args
                .get(2)
                .context("Usage: phyl say <session-id> \"message\"")?;
            let message = args
                .get(3)
                .map(|s| s.as_str())
                .context("Usage: phyl say <session-id> \"message\"")?;
            run_async(cmd_say::run(id, message))?;
        }
        "log" => {
            let id = args
                .get(2)
                .map(|s| s.as_str())
                .context("Usage: phyl log <session-id>")?;
            run_async(cmd_log::run(id))?;
        }
        "stop" => {
            let id = args
                .get(2)
                .map(|s| s.as_str())
                .context("Usage: phyl stop <session-id>")?;
            run_async(cmd_stop::run(id))?;
        }
        "watch" => {
            run_async(cmd_watch::run())?;
        }
        "ui" => {
            // Exec phyl-tui (replace current process).
            let binary =
                cmd_start::find_binary("phyl-tui").unwrap_or_else(|| "phyl-tui".to_string());
            let err = exec_replace(&binary);
            bail!("failed to exec phyl-tui: {err}");
        }
        "config" => {
            let sub_args: Vec<String> = args[2..].to_vec();
            cmd_config::run(&sub_args).context("config failed")?;
        }
        "setup" => {
            let sub_args: Vec<String> = args[2..].to_vec();
            cmd_setup::run(&sub_args).context("setup failed")?;
        }
        "help" | "--help" | "-h" => usage(),
        cmd => {
            eprintln!("phyl: unknown command '{cmd}'");
            eprintln!();
            usage();
            process::exit(1);
        }
    }

    Ok(())
}

/// Run an async function on a tokio runtime.
fn run_async(future: impl std::future::Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new().context("failed to create async runtime")?;
    rt.block_on(future)
}

fn usage() {
    eprintln!("Usage: phyl <command> [args...]");
    eprintln!();
    eprintln!("Session commands (require running daemon):");
    eprintln!("  session [-d] \"prompt\"      Start a session (-d for detached)");
    eprintln!("  ls                         List sessions");
    eprintln!("  status <id>                Session detail");
    eprintln!("  say <id> \"msg\"             Inject event into running session");
    eprintln!("  log <id>                   Tail session log");
    eprintln!("  stop <id>                  Kill session");
    eprintln!("  watch                      Live feed of all sessions, answer questions");
    eprintln!("  ui                         Interactive terminal UI");
    eprintln!();
    eprintln!("Daemon and services:");
    eprintln!("  start [-d]                Start daemon (foreground, or -d for background)");
    eprintln!("  start --all               Start all services in foreground (no systemd)");
    eprintln!();
    eprintln!("Initialization and setup:");
    eprintln!("  init [path] [--systemd]   Initialize agent home directory");
    eprintln!("  setup systemd             Generate/install/enable systemd user units");
    eprintln!("  setup status              Show health of all components");
    eprintln!("  setup migrate-xdg         Move ~/.phylactery to XDG paths");
    eprintln!();
    eprintln!("Configuration:");
    eprintln!("  config show               Pretty-print resolved config (secrets masked)");
    eprintln!("  config validate           Check config.toml for errors");
    eprintln!("  config edit               Open config.toml in $EDITOR");
    eprintln!("  config add <type> <name>  Add a config section (mcp/poll/hook/sse/watch/bridge)");
    eprintln!("  config add-secret K V     Add a secret to secrets.env");
    eprintln!("  config list-secrets       List secret keys (values masked)");
    eprintln!("  config remove-secret K    Remove a secret");
}

/// Replace the current process with the given binary (Unix exec).
fn exec_replace(binary: &str) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    // This only returns if exec fails.
    Command::new(binary).exec()
}
