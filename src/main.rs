//! swarmy: mission control for Claude Code agents.

mod claude;
mod cli;
mod config;
mod core;
mod demo;
mod index;
mod live;
mod power;
mod tui;
mod ui;
mod vault;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    if let Err(e) = cli::run(cli) {
        // Broken pipe (`swarmy report | head`) is not an error.
        if e.chain().any(|c| {
            c.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
        }) {
            std::process::exit(0);
        }
        eprintln!("swarmy: {e:#}");
        std::process::exit(1);
    }
}
