//! swarmy: mission control for Claude Code agents.

use clap::Parser;

fn main() {
    let cli = swarmy::cli::Cli::parse();
    if let Err(e) = swarmy::cli::run(cli) {
        // Broken pipe (`swarmy report | head`) is not an error.
        if e.chain().any(|c| {
            c.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
        }) {
            std::process::exit(0);
        }
        match swarmy::errors::code_of(&e) {
            Some(code) => eprintln!("swarmy: {e:#} [{code}]"),
            None => eprintln!("swarmy: {e:#}"),
        }
        std::process::exit(1);
    }
}
