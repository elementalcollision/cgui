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
pub async fn dispatch_cli(cli: &Cli) -> Result<Option<i32>> {
    if cli.args.is_empty() {
        return Ok(None);
    }
    let verb = &cli.args[0];
    if matches!(verb.as_str(), "tui" | "ui") {
        return Ok(None);
    }
    if verb == "doctor" {
        return Ok(Some(crate::doctor::run().await));
    }
    if verb == "import-compose" {
        return Ok(Some(import_compose(&cli.args[1..])));
    }
    if verb == "new" {
        return Ok(Some(new_stack(&cli.args[1..])));
    }
    if verb == "templates" {
        return Ok(Some(list_templates()));
    }
    if verb == "update" || verb == "updates" {
        // Convenience: `cgui update` runs a fresh check (bypasses cache) and
        // prints the result. Read-only — phase 1.
        return Ok(Some(check_updates_cli().await));
    }
    if verb == "--no-update" {
        // Recognised here so the dispatcher doesn't treat it as a runtime
        // verb. Effective behaviour: persists the opt-out in prefs and exits.
        let mut p = crate::prefs::Prefs::load();
        p.auto_update_check = Some(false);
        p.save();
        eprintln!("cgui: update checks disabled (auto_update_check = false in state.json)");
        return Ok(Some(0));
    }
    let mapped = translate(verb);
    let rest = &cli.args[1..];

    // Honor the saved runtime profile so e.g. `cgui ps` hits the same binary
    // the TUI does.
    let profiles = crate::runtime::load_profiles();
    let saved = crate::prefs::Prefs::load().profile;
    let want = saved
        .or_else(crate::runtime::default_name)
        .unwrap_or_else(|| profiles[0].name.clone());
    if let Some(p) = profiles.iter().find(|p| p.name == want) {
        crate::runtime::set_active(p);
    }
    let bin = crate::runtime::binary();

    let status = Command::new(&bin)
        .args(&mapped)
        .args(rest.iter().map(|s| s.as_str()))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(Some(status.code().unwrap_or(1)))
}

/// `cgui import-compose <docker-compose.yml> [--name <stack>] [--write]`
///
/// Reads the compose file, translates it to a cgui stack TOML body, and
/// either prints it to stdout (default) or writes it to
/// `~/.config/cgui/stacks/<stack>.toml` (`--write`).
fn import_compose(args: &[String]) -> i32 {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        eprintln!(
            "usage: cgui import-compose <docker-compose.yml> [--name <stack>] [--write]"
        );
        return 2;
    }
    let path = std::path::PathBuf::from(&args[0]);
    let mut name: Option<String> = None;
    let mut write = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--name needs a value");
                    return 2;
                }
                name = Some(args[i].clone());
            }
            "--write" => write = true,
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let stack_name = name.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "imported".into())
    });
    let toml = match crate::compose::import(&path, &stack_name) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("import failed: {e}");
            return 1;
        }
    };
    if !write {
        print!("{toml}");
        return 0;
    }
    let Some(target) = crate::stacks::path_for(&stack_name) else {
        eprintln!("no $XDG_CONFIG_HOME or $HOME — cannot --write");
        return 1;
    };
    if target.exists() {
        eprintln!(
            "{} already exists. Re-run with `--name <other>` or remove first.",
            target.display()
        );
        return 1;
    }
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("mkdir {}: {}", parent.display(), e);
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&target, toml) {
        eprintln!("write {}: {}", target.display(), e);
        return 1;
    }
    println!("wrote {}", target.display());
    0
}

/// `cgui new <name> [--template <kind>]` — scaffold a stack file from a
/// built-in template. Writes to `~/.config/cgui/stacks/<name>.toml`. Errors
/// if the file already exists or the template isn't known.
fn new_stack(args: &[String]) -> i32 {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        eprintln!("usage: cgui new <name> [--template <kind>]");
        eprintln!("       cgui templates    # list available templates");
        return 2;
    }
    let name = args[0].clone();
    let mut template: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--template" | "-t" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--template needs a value (try `cgui templates`)");
                    return 2;
                }
                template = Some(args[i].clone());
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
        i += 1;
    }
    match crate::stacks::create_from_template(&name, template.as_deref()) {
        Ok(p) => {
            println!("created {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("new: {e}");
            1
        }
    }
}

fn list_templates() -> i32 {
    let mut max_name = 0;
    for t in crate::stacks::TEMPLATES {
        max_name = max_name.max(t.name.len());
    }
    for t in crate::stacks::TEMPLATES {
        println!("  {:<width$}  {}", t.name, t.description, width = max_name);
    }
    println!();
    println!("usage: cgui new <name> --template <kind>");
    0
}

/// `cgui update` — force a fresh check and print findings. Bypasses the 24h
/// cache by clearing it before calling. Read-only; never installs.
async fn check_updates_cli() -> i32 {
    let mut prefs = crate::prefs::Prefs::load();
    prefs.update_cache.clear();
    let updates = crate::update::check_force(&mut prefs).await;
    if updates.is_empty() {
        println!("cgui: all components up to date");
        return 0;
    }
    for u in &updates {
        println!(
            "⬆ {:<10} {} → {}   ({})",
            u.component.label(),
            u.installed,
            u.latest,
            u.release_url
        );
    }
    0
}
