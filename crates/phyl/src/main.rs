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
        "help" | "--help" | "-h" => usage(),
        cmd => {
            // All other subcommands require the daemon (Phase 6)
            eprintln!("phyl: '{}' is not yet implemented", cmd);
            eprintln!();
            usage();
            process::exit(1);
        }
    }
}

fn usage() {
    eprintln!("Usage: phyl <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  init [path]               Initialize agent home directory");
    eprintln!("  session \"prompt\"           Start a session (not yet implemented)");
    eprintln!("  session -d \"prompt\"        Start a detached session (not yet implemented)");
    eprintln!("  ls                         List sessions (not yet implemented)");
    eprintln!("  status <id>                Session detail (not yet implemented)");
    eprintln!("  say <id> \"msg\"             Inject event (not yet implemented)");
    eprintln!("  log <id>                   Tail session log (not yet implemented)");
    eprintln!("  stop <id>                  Kill session (not yet implemented)");
    eprintln!("  watch                      Live feed of all sessions (not yet implemented)");
    eprintln!("  start [-d]                 Start daemon (not yet implemented)");
}
