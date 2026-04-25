//! Docker-compatible command shim.
//!
//! `cgui` with no subcommand → launches the TUI. With a subcommand, it
//! translates familiar Docker verbs (`ps`, `images`, `rm`, `rmi`, ...) to
//! the corresponding `container` invocation and execs through. Unknown verbs
//! are passed through untouched, so `cgui run ...` works the same as
//! `container run ...`.

use anyhow::Result;
use clap::Parser;
use std::process::{Command, Stdio};

#[derive(Parser, Debug)]
#[command(
    name = "cgui",
    version,
    about = "Visual front end + Docker-compatible shim for Apple's `container`",
    disable_help_subcommand = true,
    arg_required_else_help = false,
    trailing_var_arg = true,
    allow_hyphen_values = true
)]
pub struct Cli {
    /// Subcommand + args. If empty, launch the TUI.
    pub args: Vec<String>,
}

/// Map Docker-style verbs to Apple `container` verbs.
fn translate(verb: &str) -> Vec<&str> {
    match verb {
        "ps" => vec!["ls"],
        "images" => vec!["image", "ls"],
        "rmi" => vec!["image", "delete"],
        "pull" => vec!["image", "pull"],
        "push" => vec!["image", "push"],
        "tag" => vec!["image", "tag"],
        "login" => vec!["registry", "login"],
        "logout" => vec!["registry", "logout"],
        "network" | "networks" => vec!["network"],
        "volume" | "volumes" => vec!["volume"],
        "rm" => vec!["delete"],
        "top" => vec!["stats"],
        // Pass-through for verbs that already match (run, start, stop, kill,
        // exec, logs, build, inspect, create, stats, prune, system, ...).
        other => vec![other],
    }
}

/// Returns Some(exit_code) if a CLI command was handled; None to fall through
/// to the TUI.
pub fn dispatch_cli(cli: &Cli) -> Result<Option<i32>> {
    if cli.args.is_empty() {
        return Ok(None);
    }
    let verb = &cli.args[0];
    if matches!(verb.as_str(), "tui" | "ui") {
        return Ok(None);
    }
    let mapped = translate(verb);
    let rest = &cli.args[1..];
    let status = Command::new("container")
        .args(&mapped)
        .args(rest.iter().map(|s| s.as_str()))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(Some(status.code().unwrap_or(1)))
}
