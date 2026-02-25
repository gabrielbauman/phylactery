mod client;
mod cmd_log;
mod cmd_ls;
mod cmd_say;
mod cmd_session;
mod cmd_start;
mod cmd_status;
mod cmd_stop;
mod cmd_watch;
mod format;
mod init;

use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "init" => {
            let path = args.get(2).map(|s| s.as_str());
            if let Err(e) = init::run(path) {
                eprintln!("phyl init: {}", e);
                process::exit(1);
            }
        }
        "start" => {
            let detach = args.iter().any(|a| a == "-d");
            if let Err(e) = cmd_start::run(detach) {
                eprintln!("phyl start: {}", e);
                process::exit(1);
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
                None => {
                    eprintln!("Usage: phyl session [-d] \"prompt\"");
                    process::exit(1);
                }
            };
            run_async(cmd_session::run(prompt, detach));
        }
        "ls" => {
            run_async(cmd_ls::run());
        }
        "status" => {
            let id = match args.get(2) {
                Some(id) => id.as_str(),
                None => {
                    eprintln!("Usage: phyl status <session-id>");
                    process::exit(1);
                }
            };
            run_async(cmd_status::run(id));
        }
        "say" => {
            let id = match args.get(2) {
                Some(id) => id,
                None => {
                    eprintln!("Usage: phyl say <session-id> \"message\"");
                    process::exit(1);
                }
            };
            let message = match args.get(3) {
                Some(m) => m.as_str(),
                None => {
                    eprintln!("Usage: phyl say <session-id> \"message\"");
                    process::exit(1);
                }
            };
            run_async(cmd_say::run(id, message));
        }
        "log" => {
            let id = match args.get(2) {
                Some(id) => id.as_str(),
                None => {
                    eprintln!("Usage: phyl log <session-id>");
                    process::exit(1);
                }
            };
            run_async(cmd_log::run(id));
        }
        "stop" => {
            let id = match args.get(2) {
                Some(id) => id.as_str(),
                None => {
                    eprintln!("Usage: phyl stop <session-id>");
                    process::exit(1);
                }
            };
            run_async(cmd_stop::run(id));
        }
        "watch" => {
            run_async(cmd_watch::run());
        }
        "help" | "--help" | "-h" => usage(),
        cmd => {
            eprintln!("phyl: unknown command '{cmd}'");
            eprintln!();
            usage();
            process::exit(1);
        }
    }
}

/// Run an async function on a tokio runtime.
fn run_async(future: impl std::future::Future<Output = Result<(), String>>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(future) {
        eprintln!("phyl: {}", e);
        process::exit(1);
    }
}

fn usage() {
    eprintln!("Usage: phyl <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  init [path]               Initialize agent home directory");
    eprintln!("  start [-d]                Start daemon (foreground, or -d for background)");
    eprintln!("  session [-d] \"prompt\"      Start a session (-d for detached)");
    eprintln!("  ls                         List sessions");
    eprintln!("  status <id>                Session detail");
    eprintln!("  say <id> \"msg\"             Inject event into running session");
    eprintln!("  log <id>                   Tail session log");
    eprintln!("  stop <id>                  Kill session");
    eprintln!("  watch                      Live feed of all sessions, answer questions");
}
