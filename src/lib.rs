//! swarmy: mission control for Claude Code agents.
//!
//! The library holds everything; the `swarmy` binary only parses the command line and calls
//! [`cli::run`].

pub mod agy;
pub mod app;
pub mod claude;
pub mod cli;
pub mod config;
pub mod core;
pub mod demo;
pub mod diag;
pub mod errors;
pub mod fleet;
pub mod fsutil;
pub mod index;
pub mod limits;
pub mod live;
pub mod power;
pub mod statusline;
pub mod tui;
pub mod ui;
pub mod vault;
