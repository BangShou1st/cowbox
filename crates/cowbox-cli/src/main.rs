#![forbid(unsafe_code)]

mod agent;
mod cli;
mod commands;
mod config;
mod error;
mod json;
mod render;
mod setup;
mod state;
mod trace;
#[cfg(windows)]
mod windows_bridge;
mod workspace;

use clap::Parser;
use cli::Cli;

fn main() {
    let cli = Cli::parse();
    let trace_path = cli.command.trace_chrome().cloned();
    let chrome = match trace::init(trace_path.as_deref()) {
        Ok(chrome) => chrome,
        Err(error) => {
            error.print_human();
            std::process::exit(error.exit_code);
        }
    };

    let result = commands::dispatch(&cli.command);
    if let (Some(layer), Some(path)) = (chrome, trace_path.as_deref()) {
        if let Err(error) = layer.finish(path) {
            eprintln!(
                "cowbox: failed to write trace {}: {}",
                path.display(),
                error
            );
        }
    }

    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if cli.command.json() {
                let value = json::error(cli.command.name(), &error);
                let _ = json::print(&value);
            } else {
                error.print_human();
            }
            std::process::exit(error.exit_code);
        }
    }
}
